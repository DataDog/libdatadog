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

mod trace;
pub use trace::*;

mod operation;
use operation::*;

mod buffer;
pub use buffer::*;

pub mod span_header;
pub use span_header::{SpanHeader, SPAN_HEADER_SIZE};

use crate::span::v04::Span;
use crate::span::vec_map::VecMap;
use crate::span::{SpanText, TraceData};

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
    spans: Vec<Option<Span<T>>>,
    traces: SmallTraceMap<T::Text>,
    string_table: StringTable<T::Text>,
    tracer_service: T::Text,
    tracer_language: T::Text,
    pid: u32,
    /// Default meta tags automatically applied to every new span via create_span.
    default_meta: Vec<(T::Text, T::Text)>,
    /// Contiguous array of span headers for direct JS DataView access. JS writes numeric and
    /// string-ID fields directly here. Rust reads them during flush_chunk.
    pub span_headers: Vec<SpanHeader>,
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

fn span_at_mut<T: TraceData>(spans: &mut [Option<Span<T>>], slot: usize) -> Result<&mut Span<T>> {
    spans
        .get_mut(slot)
        .and_then(|opt| opt.as_mut())
        .ok_or(ChangeBufferError::SpanNotFound(slot as u64))
}

impl<T: TraceData> ChangeBufferState<T>
where
    T::Text: Clone,
{
    /// The maximun size of the recycled span pool, beyond which we don't recycle spans anymore but
    /// drop them.
    const SPANS_POOL_MAX_SIZE: usize = 128;

    pub fn new(
        change_buffer: ChangeBuffer,
        tracer_service: T::Text,
        tracer_language: T::Text,
        pid: u32,
    ) -> Self {
        ChangeBufferState {
            change_buffer,
            spans: Vec::with_capacity(256),
            traces: SmallTraceMap::default(),
            string_table: StringTable::with_capacity(256),
            tracer_service,
            tracer_language,
            pid,
            default_meta: Vec::new(),
            span_headers: Vec::with_capacity(256),
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
        self.spans.iter().filter(|s| s.is_some()).count()
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
        slot_indices: &[u32],
        first_is_local_root: bool,
    ) -> Result<Vec<Span<T>>> {
        let mut is_local_root = first_is_local_root;
        let mut is_chunk_root = true;

        let mut spans_vec = Vec::with_capacity(slot_indices.len());
        // Fetch the trace_id corresponding to this chunk. It must be the same for all the spans in
        // the chunk.
        let Some(trace_id) = slot_indices
            .first()
            .map(|idx| span_at_mut(&mut self.spans, *idx as usize))
            .transpose()?
            .map(|span| span.trace_id)
        else {
            return Ok(vec![]);
        };

        let trace = self.traces.get(&trace_id);

        for slot in slot_indices {
            let maybe_span = self
                .spans
                .get_mut(*slot as usize)
                .and_then(|opt| opt.take());

            let mut span = maybe_span.ok_or(ChangeBufferError::SpanNotFound(*slot as u64))?;

            if is_local_root {
                self.copy_in_sampling_tags(trace, &mut span);
                span.metrics.insert(self.str_top_level.clone(), 1.0);
                is_local_root = false;
            }

            if is_chunk_root {
                Self::copy_in_chunk_tags(trace, &mut span);
                is_chunk_root = false;
            }

            self.process_span(trace, &mut span);

            spans_vec.push(span);
        }

        let trace = self.traces.get_mut(&trace_id);

        let should_remove = trace
            .map(|trace| {
                if trace.span_count <= spans_vec.len() {
                    true
                } else {
                    trace.span_count -= spans_vec.len();
                    false
                }
            })
            .unwrap_or(false);

        if should_remove {
            self.traces.remove(&trace_id);
        }

        Ok(spans_vec)
    }

    fn copy_in_sampling_tags(&self, trace: Option<&Trace<T::Text>>, span: &mut Span<T>) {
        if let Some(trace) = trace {
            if let Some(rule) = trace.sampling_rule_decision {
                span.metrics.insert(self.str_rule_psr.clone(), rule);
            }
            if let Some(rule) = trace.sampling_limit_decision {
                span.metrics.insert(self.str_limit_psr.clone(), rule);
            }
            if let Some(rule) = trace.sampling_agent_decision {
                span.metrics.insert(self.str_agent_psr.clone(), rule);
            }
        }
    }

    fn copy_in_chunk_tags(trace: Option<&Trace<T::Text>>, span: &mut Span<T>) {
        if let Some(trace) = trace {
            span.meta
                .extend(trace.meta.iter().map(|(k, v)| (k.clone(), v.clone())));
            span.metrics
                .extend(trace.metrics.iter().map(|(k, v)| (k.clone(), *v)));
        }
    }

    fn process_span(&self, trace: Option<&Trace<T::Text>>, span: &mut Span<T>) {
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

        if let Some(trace) = trace {
            if let Some(origin) = trace.origin.clone() {
                span.meta.insert(self.str_origin.clone(), origin);
            }
        }
    }

    pub fn flush_change_buffer(&mut self) -> Result<()> {
        let mut index = 0;
        let mut count = self.change_buffer.read::<u64>(&mut index)? as u32;

        while count > 0 {
            let op = BufferedOperation::from_buf(&self.change_buffer, &mut index)?;

            match op.opcode {
                OpCode::Create | OpCode::CreateSpan | OpCode::CreateSpanFull => {
                    self.interpret_operation(&mut index, &op)?;
                }
                _ => {
                    self.interpret_operation_cached(&mut index, &op)?;
                }
            }
            count -= 1;
        }

        self.change_buffer.write_u32(0, 0)?;
        self.change_buffer.write_u32(4, 0)?;

        Ok(())
    }

    fn interpret_operation_cached(
        &mut self,
        index: &mut usize,
        op: &BufferedOperation,
    ) -> Result<()> {
        let slot = op.slot_index as usize;
        let buf = &self.change_buffer;
        let span = self
            .spans
            .get_mut(slot)
            .and_then(|opt| opt.as_mut())
            .ok_or(ChangeBufferError::SpanNotFound(slot as u64))?;

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
                let trace_id = span.trace_id;
                if let Some(trace) = self.traces.get_mut(&trace_id) {
                    trace.meta.insert(name, val);
                }
            }
            OpCode::SetTraceMetricsAttr => {
                let name = buf.read_string(&self.string_table, index)?;
                let val = buf.read(index)?;
                let trace_id = span.trace_id;
                if let Some(trace) = self.traces.get_mut(&trace_id) {
                    trace.metrics.insert(name, val);
                }
            }
            OpCode::SetTraceOrigin => {
                let origin = buf.read_string(&self.string_table, index)?;
                let trace_id = span.trace_id;
                if let Some(trace) = self.traces.get_mut(&trace_id) {
                    trace.origin = Some(origin);
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
            OpCode::Create | OpCode::CreateSpan | OpCode::CreateSpanFull => unreachable!(),
        }

        Ok(())
    }

    pub fn get_span(&self, slot: u32) -> Result<&Span<T>> {
        self.spans
            .get(slot as usize)
            .and_then(|opt| opt.as_ref())
            .ok_or(ChangeBufferError::SpanNotFound(slot as u64))
    }

    #[inline]
    pub fn get_trace(&self, id: &u128) -> Option<&Trace<T::Text>> {
        self.traces.get(id)
    }

    pub fn materialize_header(&mut self, header_idx: u32) -> Result<u64> {
        let h = &self.span_headers[header_idx as usize];
        if h.active == 0 {
            return Err(ChangeBufferError::SpanNotFound(0));
        }
        let span_id = h.span_id;
        let trace_id = (h.trace_id_hi as u128) << 64 | h.trace_id_lo as u128;
        let parent_id = h.parent_id;

        let mut span = new_span_pooled(&mut self.span_pool, span_id, parent_id, trace_id);
        span.start = h.start;
        span.duration = h.duration;
        span.error = h.error;

        if let Some(name) = self.string_table.get(h.name_id) {
            span.name = name;
        }
        if let Some(service) = self.string_table.get(h.service_id) {
            span.service = service;
        }
        if let Some(resource) = self.string_table.get(h.resource_id) {
            span.resource = resource;
        }
        if let Some(r#type) = self.string_table.get(h.type_id) {
            span.r#type = r#type;
        }

        self.apply_default_meta(&mut span);

        let mut found_slot = None;
        for (i, opt) in self.spans.iter().enumerate() {
            if let Some(ref s) = opt {
                if s.span_id == span_id {
                    found_slot = Some(i);
                    break;
                }
            }
        }

        if let Some(slot) = found_slot {
            #[allow(clippy::unwrap_used)]
            let existing = self.spans[slot].as_mut().unwrap();
            existing.start = span.start;
            existing.duration = span.duration;
            existing.error = span.error;
            existing.name = span.name;
            existing.service = span.service;
            existing.resource = span.resource;
            existing.r#type = span.r#type;
            existing.trace_id = span.trace_id;
            existing.parent_id = span.parent_id;
            existing.meta.extend(self.default_meta.iter().cloned());
        } else {
            self.spans.push(Some(span));
            self.traces.get_or_insert_default(trace_id).span_count += 1;
        }
        self.span_headers[header_idx as usize].active = 0;

        Ok(span_id)
    }

    pub fn span_mut(&mut self, slot: &u32) -> Result<&mut Span<T>> {
        self.spans
            .get_mut(*slot as usize)
            .and_then(|opt| opt.as_mut())
            .ok_or(ChangeBufferError::SpanNotFound(*slot as u64))
    }

    #[inline]
    pub fn set_default_meta(&mut self, tags: Vec<(T::Text, T::Text)>) {
        self.default_meta = tags;
    }

    fn apply_default_meta(&self, span: &mut Span<T>) {
        for (key, value) in &self.default_meta {
            span.meta.insert(key.clone(), value.clone());
        }
    }

    fn ensure_slot(&mut self, slot: u32) {
        let idx = slot as usize;
        if idx >= self.spans.len() {
            self.spans.resize_with(idx + 1, || None);
        }
    }

    fn interpret_operation(&mut self, index: &mut usize, op: &BufferedOperation) -> Result<()> {
        let slot = op.slot_index as usize;
        let buf = &self.change_buffer;

        match op.opcode {
            OpCode::Create => {
                let span_id: u64 = buf.read(index)?;
                let trace_id: u128 = buf.read(index)?;
                let parent_id = buf.read(index)?;
                let mut span = new_span_pooled(&mut self.span_pool, span_id, parent_id, trace_id);
                self.apply_default_meta(&mut span);
                self.ensure_slot(op.slot_index);
                self.spans[slot] = Some(span);
                self.traces.get_or_insert_default(trace_id).span_count += 1;
            }
            OpCode::SetMetaAttr => {
                let key = buf.read_string(&self.string_table, index)?;
                let val = buf.read_string(&self.string_table, index)?;
                span_at_mut(&mut self.spans, slot)?.meta.insert(key, val);
            }
            OpCode::SetMetricAttr => {
                let key = buf.read_string(&self.string_table, index)?;
                let val: f64 = buf.read(index)?;
                span_at_mut(&mut self.spans, slot)?.metrics.insert(key, val);
            }
            OpCode::SetServiceName => {
                let service = buf.read_string(&self.string_table, index)?;
                span_at_mut(&mut self.spans, slot)?.service = service;
            }
            OpCode::SetResourceName => {
                let resource = buf.read_string(&self.string_table, index)?;
                span_at_mut(&mut self.spans, slot)?.resource = resource;
            }
            OpCode::SetError => {
                let error = buf.read(index)?;
                span_at_mut(&mut self.spans, slot)?.error = error;
            }
            OpCode::SetStart => {
                let start = buf.read(index)?;
                span_at_mut(&mut self.spans, slot)?.start = start;
            }
            OpCode::SetDuration => {
                let duration = buf.read(index)?;
                span_at_mut(&mut self.spans, slot)?.duration = duration;
            }
            OpCode::SetType => {
                let r#type = buf.read_string(&self.string_table, index)?;
                span_at_mut(&mut self.spans, slot)?.r#type = r#type;
            }
            OpCode::SetName => {
                let name = buf.read_string(&self.string_table, index)?;
                span_at_mut(&mut self.spans, slot)?.name = name;
            }
            OpCode::SetTraceMetaAttr => {
                let name = buf.read_string(&self.string_table, index)?;
                let val = buf.read_string(&self.string_table, index)?;
                let trace_id = span_at_mut(&mut self.spans, slot)?.trace_id;
                if let Some(trace) = self.traces.get_mut(&trace_id) {
                    trace.meta.insert(name, val);
                }
            }
            OpCode::SetTraceMetricsAttr => {
                let name = buf.read_string(&self.string_table, index)?;
                let val = buf.read(index)?;
                let trace_id = span_at_mut(&mut self.spans, slot)?.trace_id;
                if let Some(trace) = self.traces.get_mut(&trace_id) {
                    trace.metrics.insert(name, val);
                }
            }
            OpCode::SetTraceOrigin => {
                let origin = buf.read_string(&self.string_table, index)?;
                let trace_id = span_at_mut(&mut self.spans, slot)?.trace_id;
                if let Some(trace) = self.traces.get_mut(&trace_id) {
                    trace.origin = Some(origin);
                }
            }
            OpCode::CreateSpan => {
                let span_id: u64 = buf.read(index)?;
                let trace_id: u128 = buf.read(index)?;
                let parent_id: u64 = buf.read(index)?;
                let name = buf.read_string(&self.string_table, index)?;
                let start: i64 = buf.read(index)?;
                let mut span = new_span_pooled(&mut self.span_pool, span_id, parent_id, trace_id);
                span.name = name;
                span.start = start;
                self.apply_default_meta(&mut span);
                self.ensure_slot(op.slot_index);
                self.spans[slot] = Some(span);
                self.traces.get_or_insert_default(trace_id).span_count += 1;
            }
            OpCode::CreateSpanFull => {
                let span_id: u64 = buf.read(index)?;
                let trace_id: u128 = buf.read(index)?;
                let parent_id: u64 = buf.read(index)?;
                let name = buf.read_string(&self.string_table, index)?;
                let service = buf.read_string(&self.string_table, index)?;
                let resource = buf.read_string(&self.string_table, index)?;
                let r#type = buf.read_string(&self.string_table, index)?;
                let start: i64 = buf.read(index)?;
                let mut span = new_span_pooled(&mut self.span_pool, span_id, parent_id, trace_id);
                span.name = name;
                span.service = service;
                span.resource = resource;
                span.r#type = r#type;
                span.start = start;
                self.apply_default_meta(&mut span);
                self.ensure_slot(op.slot_index);
                self.spans[slot] = Some(span);
                self.traces.get_or_insert_default(trace_id).span_count += 1;
            }
            OpCode::BatchSetMeta => {
                let count: u32 = buf.read(index)?;
                let span = span_at_mut(&mut self.spans, slot)?;
                for _ in 0..count {
                    let key = buf.read_string(&self.string_table, index)?;
                    let val = buf.read_string(&self.string_table, index)?;
                    span.meta.insert(key, val);
                }
            }
            OpCode::BatchSetMetric => {
                let count: u32 = buf.read(index)?;
                let span = span_at_mut(&mut self.spans, slot)?;
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
