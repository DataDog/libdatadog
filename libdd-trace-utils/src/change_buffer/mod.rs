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
#![allow(dead_code)]

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
use utils::*;

mod trace;
pub use trace::*;

mod operation;
use operation::*;

mod buffer;
pub use buffer::*;

pub mod span_header;
pub use span_header::{SpanHeader, SPAN_HEADER_SIZE};

use crate::span::v04::Span;
use crate::span::{SpanText, TraceData};

fn vec_insert<K: PartialEq, V>(vec: &mut Vec<(K, V)>, key: K, value: V) {
    for entry in vec.iter_mut() {
        if entry.0 == key {
            entry.1 = value;
            return;
        }
    }
    vec.push((key, value));
}

fn vec_get<'a, K: PartialEq, V>(vec: &'a [(K, V)], key: &K) -> Option<&'a V> {
    for entry in vec {
        if entry.0 == *key {
            return Some(&entry.1);
        }
    }
    None
}

fn deferred_meta_insert(vec: &mut Vec<(u32, u32)>, key_id: u32, val_id: u32) {
    for entry in vec.iter_mut() {
        if entry.0 == key_id {
            entry.1 = val_id;
            return;
        }
    }
    vec.push((key_id, val_id));
}

fn deferred_metric_insert(vec: &mut Vec<(u32, f64)>, key_id: u32, val: f64) {
    for entry in vec.iter_mut() {
        if entry.0 == key_id {
            entry.1 = val;
            return;
        }
    }
    vec.push((key_id, val));
}

pub struct ChangeBufferState<T: TraceData> {
    change_buffer: ChangeBuffer,
    spans: Vec<Option<Span<T>>>,
    traces: SmallTraceMap<T::Text>,
    /// String table indexed by sequential u32 IDs (O(1) lookup vs HashMap).
    string_table: Vec<Option<T::Text>>,
    tracer_service: T::Text,
    tracer_language: T::Text,
    pid: u32,
    /// Default meta tags automatically applied to every new span via create_span.
    default_meta: Vec<(T::Text, T::Text)>,
    /// Contiguous array of span headers for direct JS DataView access.
    /// JS writes numeric and string-ID fields directly here. Rust reads
    /// them during flush_chunk.
    pub span_headers: Vec<SpanHeader>,
    /// Free list of recycled header indices (from finished spans).
    header_free_list: Vec<u32>,
    // Cached static strings to avoid repeated heap allocations (e.g. Arc<str>)
    // on every span flush. These are created once and cloned (cheap ref bump).
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
    /// Pool of recycled Span objects. Reusing spans (with their pre-allocated
    /// Vec buffers) eliminates the alloc/dealloc churn that fragments the
    /// WASM linear memory allocator over time.
    span_pool: Vec<Span<T>>,
    /// Deferred meta tags: indexed by slot, stores (key_string_id, val_string_id) pairs.
    deferred_meta: Vec<Vec<(u32, u32)>>,
    /// Deferred metric tags: indexed by slot, stores (key_string_id, f64_value) pairs.
    deferred_metrics: Vec<Vec<(u32, f64)>>,
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
            meta: Vec::with_capacity(8),
            metrics: Vec::with_capacity(4),
            ..Default::default()
        }
    }
}

impl<T: TraceData> ChangeBufferState<T>
where
    T::Text: Clone,
{
    pub fn new(
        change_buffer: ChangeBuffer,
        tracer_service: T::Text,
        tracer_language: T::Text,
        pid: u32,
    ) -> Self {
        eprintln!(
            "[libdatadog pipeline] experiment: baseline (commit: {})",
            env!("GIT_COMMIT")
        );
        ChangeBufferState {
            change_buffer,
            spans: Vec::with_capacity(256),
            traces: SmallTraceMap::default(),
            string_table: Vec::with_capacity(256),
            tracer_service,
            tracer_language,
            pid,
            default_meta: Vec::new(),
            span_headers: Vec::with_capacity(256),
            header_free_list: Vec::new(),
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
            deferred_meta: Vec::with_capacity(256),
            deferred_metrics: Vec::with_capacity(256),
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
        let available = 128usize.saturating_sub(self.span_pool.len());
        for span in spans.into_iter().take(available) {
            self.span_pool.push(span);
        }
    }

    pub fn flush_chunk(
        &mut self,
        slot_indices: Vec<u32>,
        first_is_local_root: bool,
    ) -> Result<Vec<Span<T>>> {
        let mut chunk_trace_id: Option<u128> = None;
        let mut is_local_root = first_is_local_root;
        let mut is_chunk_root = true;

        let mut spans_vec = Vec::with_capacity(slot_indices.len());
        for slot in &slot_indices {
            let maybe_span = self
                .spans
                .get_mut(*slot as usize)
                .and_then(|opt| opt.take());

            let mut span = maybe_span.ok_or(ChangeBufferError::SpanNotFound(*slot as u64))?;

            self.materialize_deferred_tags(*slot, &mut span);

            chunk_trace_id = Some(span.trace_id);

            if is_local_root {
                self.copy_in_sampling_tags(&mut span);
                vec_insert(&mut span.metrics, self.str_top_level.clone(), 1.0);
                is_local_root = false;
            }
            if is_chunk_root {
                self.copy_in_chunk_tags(&mut span);
                is_chunk_root = false;
            }

            self.process_one_span(&mut span);

            spans_vec.push(span);
        }

        let mut seen_trace_ids: Vec<(u128, usize)> = Vec::new();
        for span in &spans_vec {
            if let Some(entry) = seen_trace_ids.iter_mut().find(|(id, _)| *id == span.trace_id) {
                entry.1 += 1;
            } else {
                seen_trace_ids.push((span.trace_id, 1));
            }
        }
        for (trace_id, flushed_count) in seen_trace_ids {
            let should_remove = self
                .traces
                .get_mut(&trace_id)
                .map(|trace| {
                    if trace.span_count <= flushed_count {
                        true
                    } else {
                        trace.span_count -= flushed_count;
                        false
                    }
                })
                .unwrap_or(false);
            if should_remove {
                self.traces.remove(&trace_id);
            }
        }

        Ok(spans_vec)
    }

    fn copy_in_sampling_tags(&self, span: &mut Span<T>) {
        if let Some(trace) = self.traces.get(&span.trace_id) {
            if let Some(rule) = trace.sampling_rule_decision {
                vec_insert(&mut span.metrics, self.str_rule_psr.clone(), rule);
            }
            if let Some(rule) = trace.sampling_limit_decision {
                vec_insert(&mut span.metrics, self.str_limit_psr.clone(), rule);
            }
            if let Some(rule) = trace.sampling_agent_decision {
                vec_insert(&mut span.metrics, self.str_agent_psr.clone(), rule);
            }
        }
    }

    fn copy_in_chunk_tags(&self, span: &mut Span<T>) {
        if let Some(trace) = self.traces.get(&span.trace_id) {
            span.meta.reserve(trace.meta.len());
            for (k, v) in &trace.meta {
                vec_insert(&mut span.meta, k.clone(), v.clone());
            }
            span.metrics.reserve(trace.metrics.len());
            for (k, v) in &trace.metrics {
                vec_insert(&mut span.metrics, k.clone(), *v);
            }
        }
    }

    fn process_one_span(&self, span: &mut Span<T>) {
        let kind_key = T::Text::from_static_str("kind");
        if let Some(kind) = vec_get(&span.meta, &kind_key) {
            if *kind != self.str_internal {
                vec_insert(&mut span.metrics, self.str_measured.clone(), 1.0);
            }
        }

        if span.service != self.tracer_service {
            vec_insert(
                &mut span.meta,
                self.str_base_service.clone(),
                self.tracer_service.clone(),
            );
        }

        vec_insert(
            &mut span.meta,
            self.str_language.clone(),
            self.tracer_language.clone(),
        );
        vec_insert(
            &mut span.metrics,
            self.str_process_id.clone(),
            f64::from(self.pid),
        );

        if let Some(trace) = self.traces.get(&span.trace_id) {
            if let Some(origin) = trace.origin.clone() {
                vec_insert(&mut span.meta, self.str_origin.clone(), origin);
            }
        }
    }

    pub fn flush_change_buffer(&mut self) -> Result<()> {
        let mut index = 0;
        let mut count = self.change_buffer.read::<u64>(&mut index)? as u32;

        let mut cached_slot: u32 = u32::MAX;
        let mut cached_span_ptr: *mut Span<T> = std::ptr::null_mut();
        let mut cached_deferred_meta: *mut Vec<(u32, u32)> = std::ptr::null_mut();
        let mut cached_deferred_metrics: *mut Vec<(u32, f64)> = std::ptr::null_mut();

        while count > 0 {
            let op = BufferedOperation::from_buf(&self.change_buffer, &mut index)?;

            match op.opcode {
                OpCode::Create | OpCode::CreateSpan | OpCode::CreateSpanFull => {
                    cached_span_ptr = std::ptr::null_mut();
                    cached_slot = u32::MAX;
                    cached_deferred_meta = std::ptr::null_mut();
                    cached_deferred_metrics = std::ptr::null_mut();
                    self.interpret_operation(&mut index, &op)?;
                }
                _ => {
                    self.interpret_operation_cached(
                        &mut index,
                        &op,
                        &mut cached_slot,
                        &mut cached_span_ptr,
                        &mut cached_deferred_meta,
                        &mut cached_deferred_metrics,
                    )?;
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
        cached_slot: &mut u32,
        cached_span_ptr: &mut *mut Span<T>,
        cached_deferred_meta: &mut *mut Vec<(u32, u32)>,
        cached_deferred_metrics: &mut *mut Vec<(u32, f64)>,
    ) -> Result<()> {
        let span_ptr = if op.slot_index == *cached_slot && !cached_span_ptr.is_null() {
            *cached_span_ptr
        } else {
            let slot = op.slot_index as usize;
            let span = self
                .spans
                .get_mut(slot)
                .and_then(|opt| opt.as_mut())
                .ok_or(ChangeBufferError::SpanNotFound(op.slot_index as u64))?
                as *mut Span<T>;
            *cached_slot = op.slot_index;
            *cached_span_ptr = span;
            *cached_deferred_meta = &mut self.deferred_meta[slot] as *mut Vec<(u32, u32)>;
            *cached_deferred_metrics = &mut self.deferred_metrics[slot] as *mut Vec<(u32, f64)>;
            span
        };

        // SAFETY: span_ptr is valid — it was obtained from self.spans above
        // or from the cache which was set in a previous iteration of the same loop.
        // self.spans is not modified during this function (no inserts/removes).
        let span = unsafe { &mut *span_ptr };

        match op.opcode {
            OpCode::SetMetaAttr => {
                let key_id: u32 = self.get_num_arg(index)?;
                let val_id: u32 = self.get_num_arg(index)?;
                let dm = unsafe { &mut **cached_deferred_meta };
                deferred_meta_insert(dm, key_id, val_id);
            }
            OpCode::SetMetricAttr => {
                let key_id: u32 = self.get_num_arg(index)?;
                let val: f64 = self.get_num_arg(index)?;
                let dm = unsafe { &mut **cached_deferred_metrics };
                deferred_metric_insert(dm, key_id, val);
            }
            OpCode::SetServiceName => {
                span.service = unsafe { self.get_string_arg_unchecked(index) };
            }
            OpCode::SetResourceName => {
                span.resource = unsafe { self.get_string_arg_unchecked(index) };
            }
            OpCode::SetError => {
                span.error = unsafe { self.get_num_arg_unchecked(index) };
            }
            OpCode::SetStart => {
                span.start = unsafe { self.get_num_arg_unchecked(index) };
            }
            OpCode::SetDuration => {
                span.duration = unsafe { self.get_num_arg_unchecked(index) };
            }
            OpCode::SetType => {
                span.r#type = unsafe { self.get_string_arg_unchecked(index) };
            }
            OpCode::SetName => {
                span.name = unsafe { self.get_string_arg_unchecked(index) };
            }
            OpCode::SetTraceMetaAttr => {
                let name = self.get_string_arg(index)?;
                let val = self.get_string_arg(index)?;
                let trace_id = span.trace_id;
                if let Some(trace) = self.traces.get_mut(&trace_id) {
                    vec_insert(&mut trace.meta, name, val);
                }
            }
            OpCode::SetTraceMetricsAttr => {
                let name = self.get_string_arg(index)?;
                let val = self.get_num_arg(index)?;
                let trace_id = span.trace_id;
                if let Some(trace) = self.traces.get_mut(&trace_id) {
                    vec_insert(&mut trace.metrics, name, val);
                }
            }
            OpCode::SetTraceOrigin => {
                let origin = self.get_string_arg(index)?;
                let trace_id = span.trace_id;
                if let Some(trace) = self.traces.get_mut(&trace_id) {
                    trace.origin = Some(origin);
                }
            }
            OpCode::BatchSetMeta => {
                let count: u32 = self.get_num_arg(index)?;
                let dm = unsafe { &mut **cached_deferred_meta };
                for _ in 0..count {
                    let key_id: u32 = self.get_num_arg(index)?;
                    let val_id: u32 = self.get_num_arg(index)?;
                    deferred_meta_insert(dm, key_id, val_id);
                }
            }
            OpCode::BatchSetMetric => {
                let count: u32 = self.get_num_arg(index)?;
                let dm = unsafe { &mut **cached_deferred_metrics };
                for _ in 0..count {
                    let key_id: u32 = self.get_num_arg(index)?;
                    let val: f64 = self.get_num_arg(index)?;
                    deferred_metric_insert(dm, key_id, val);
                }
            }
            OpCode::Create | OpCode::CreateSpan | OpCode::CreateSpanFull => unreachable!(),
        }

        Ok(())
    }

    fn get_string_arg(&self, index: &mut usize) -> Result<T::Text> {
        let num: u32 = self.get_num_arg(index)?;
        self.string_table
            .get(num as usize)
            .and_then(|opt| opt.clone())
            .ok_or(ChangeBufferError::StringNotFound(num))
    }

    #[inline(always)]
    unsafe fn get_string_arg_unchecked(&self, index: &mut usize) -> T::Text {
        let num: u32 = self.change_buffer.read_unchecked(index);
        self.string_table
            .get_unchecked(num as usize)
            .clone()
            .unwrap_unchecked()
    }

    fn get_num_arg<U: Copy + FromBytes>(&self, index: &mut usize) -> Result<U> {
        self.change_buffer.read(index)
    }

    #[inline(always)]
    unsafe fn get_num_arg_unchecked<U: Copy + FromBytes>(&self, index: &mut usize) -> U {
        self.change_buffer.read_unchecked(index)
    }

    fn get_mut_span(&mut self, slot: u32) -> Result<&mut Span<T>> {
        self.spans
            .get_mut(slot as usize)
            .and_then(|opt| opt.as_mut())
            .ok_or(ChangeBufferError::SpanNotFound(slot as u64))
    }

    pub fn get_span(&self, slot: u32) -> Result<&Span<T>> {
        self.spans
            .get(slot as usize)
            .and_then(|opt| opt.as_ref())
            .ok_or(ChangeBufferError::SpanNotFound(slot as u64))
    }

    pub fn get_trace(&self, id: &u128) -> Option<&Trace<T::Text>> {
        self.traces.get(id)
    }

    pub fn alloc_header(&mut self) -> u32 {
        if let Some(idx) = self.header_free_list.pop() {
            self.span_headers[idx as usize] = SpanHeader::default();
            self.span_headers[idx as usize].active = 1;
            idx
        } else {
            let idx = self.span_headers.len() as u32;
            let mut header = SpanHeader::default();
            header.active = 1;
            self.span_headers.push(header);
            idx
        }
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

        if let Some(name) = self.get_string(h.name_id) {
            span.name = name;
        }
        if let Some(service) = self.get_string(h.service_id) {
            span.service = service;
        }
        if let Some(resource) = self.get_string(h.resource_id) {
            span.resource = resource;
        }
        if let Some(r#type) = self.get_string(h.type_id) {
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
            for (k, v) in &self.default_meta {
                vec_insert(&mut existing.meta, k.clone(), v.clone());
            }
        } else {
            let slot_idx = self.spans.len();
            self.spans.push(Some(span));
            if slot_idx >= self.deferred_meta.len() {
                self.deferred_meta.resize_with(slot_idx + 1, Vec::new);
                self.deferred_metrics.resize_with(slot_idx + 1, Vec::new);
            }
        }

        self.traces.get_or_insert_default(trace_id).span_count += 1;

        self.span_headers[header_idx as usize].active = 0;
        self.header_free_list.push(header_idx);

        Ok(span_id)
    }

    pub fn span_mut(&mut self, slot: &u32) -> Result<&mut Span<T>> {
        self.spans
            .get_mut(*slot as usize)
            .and_then(|opt| opt.as_mut())
            .ok_or(ChangeBufferError::SpanNotFound(*slot as u64))
    }

    pub fn get_string(&self, id: u32) -> Option<T::Text> {
        self.string_table
            .get(id as usize)
            .and_then(|opt| opt.clone())
    }

    pub fn set_default_meta(&mut self, tags: Vec<(T::Text, T::Text)>) {
        self.default_meta = tags;
    }

    fn apply_default_meta(&self, span: &mut Span<T>) {
        for (key, value) in &self.default_meta {
            vec_insert(&mut span.meta, key.clone(), value.clone());
        }
    }

    fn materialize_deferred_tags(&mut self, slot: u32, span: &mut Span<T>) {
        let idx = slot as usize;
        if idx < self.deferred_meta.len() {
            let pairs: Vec<(u32, u32)> = self.deferred_meta[idx].drain(..).collect();
            for (key_id, val_id) in pairs {
                if let (Some(key), Some(val)) = (self.get_string(key_id), self.get_string(val_id))
                {
                    vec_insert(&mut span.meta, key, val);
                }
            }
        }
        if idx < self.deferred_metrics.len() {
            let pairs: Vec<(u32, f64)> = self.deferred_metrics[idx].drain(..).collect();
            for (key_id, val) in pairs {
                if let Some(key) = self.get_string(key_id) {
                    vec_insert(&mut span.metrics, key, val);
                }
            }
        }
    }

    pub fn materialize_slot(&mut self, slot: u32) {
        let idx = slot as usize;
        let mut meta_pairs: Vec<(T::Text, T::Text)> = Vec::new();
        let mut metric_pairs: Vec<(T::Text, f64)> = Vec::new();

        if idx < self.deferred_meta.len() {
            for &(key_id, val_id) in &self.deferred_meta[idx] {
                if let (Some(key), Some(val)) = (self.get_string(key_id), self.get_string(val_id))
                {
                    meta_pairs.push((key, val));
                }
            }
            self.deferred_meta[idx].clear();
        }
        if idx < self.deferred_metrics.len() {
            for &(key_id, val) in &self.deferred_metrics[idx] {
                if let Some(key) = self.get_string(key_id) {
                    metric_pairs.push((key, val));
                }
            }
            self.deferred_metrics[idx].clear();
        }

        if let Some(Some(span)) = self.spans.get_mut(idx) {
            for (k, v) in meta_pairs {
                vec_insert(&mut span.meta, k, v);
            }
            for (k, v) in metric_pairs {
                vec_insert(&mut span.metrics, k, v);
            }
        }
    }

    fn ensure_slot(&mut self, slot: u32) {
        let idx = slot as usize;
        if idx >= self.spans.len() {
            self.spans.resize_with(idx + 1, || None);
        }
        if idx >= self.deferred_meta.len() {
            self.deferred_meta.resize_with(idx + 1, Vec::new);
            self.deferred_metrics.resize_with(idx + 1, Vec::new);
        }
    }

    fn interpret_operation(&mut self, index: &mut usize, op: &BufferedOperation) -> Result<()> {
        match op.opcode {
            OpCode::Create => {
                let span_id: u64 = self.change_buffer.read(index)?;
                let trace_id: u128 = self.change_buffer.read(index)?;
                let parent_id = self.get_num_arg(index)?;
                let mut span =
                    new_span_pooled(&mut self.span_pool, span_id, parent_id, trace_id);
                self.apply_default_meta(&mut span);
                self.ensure_slot(op.slot_index);
                self.spans[op.slot_index as usize] = Some(span);
                self.deferred_meta[op.slot_index as usize].clear();
                self.deferred_metrics[op.slot_index as usize].clear();
                self.traces.get_or_insert_default(trace_id).span_count += 1;
            }
            OpCode::SetMetaAttr => {
                let key_id: u32 = self.get_num_arg(index)?;
                let val_id: u32 = self.get_num_arg(index)?;
                let idx = op.slot_index as usize;
                if idx < self.deferred_meta.len() {
                    deferred_meta_insert(&mut self.deferred_meta[idx], key_id, val_id);
                }
            }
            OpCode::SetMetricAttr => {
                let key_id: u32 = self.get_num_arg(index)?;
                let val: f64 = self.get_num_arg(index)?;
                let idx = op.slot_index as usize;
                if idx < self.deferred_metrics.len() {
                    deferred_metric_insert(&mut self.deferred_metrics[idx], key_id, val);
                }
            }
            OpCode::SetServiceName => {
                self.get_mut_span(op.slot_index)?.service = self.get_string_arg(index)?;
            }
            OpCode::SetResourceName => {
                self.get_mut_span(op.slot_index)?.resource = self.get_string_arg(index)?;
            }
            OpCode::SetError => {
                self.get_mut_span(op.slot_index)?.error = self.get_num_arg(index)?;
            }
            OpCode::SetStart => {
                self.get_mut_span(op.slot_index)?.start = self.get_num_arg(index)?;
            }
            OpCode::SetDuration => {
                self.get_mut_span(op.slot_index)?.duration = self.get_num_arg(index)?;
            }
            OpCode::SetType => {
                self.get_mut_span(op.slot_index)?.r#type = self.get_string_arg(index)?;
            }
            OpCode::SetName => {
                self.get_mut_span(op.slot_index)?.name = self.get_string_arg(index)?;
            }
            OpCode::SetTraceMetaAttr => {
                let name = self.get_string_arg(index)?;
                let val = self.get_string_arg(index)?;
                let trace_id = self.get_span(op.slot_index)?.trace_id;
                if let Some(trace) = self.traces.get_mut(&trace_id) {
                    vec_insert(&mut trace.meta, name, val);
                }
            }
            OpCode::SetTraceMetricsAttr => {
                let name = self.get_string_arg(index)?;
                let val = self.get_num_arg(index)?;
                let trace_id = self.get_span(op.slot_index)?.trace_id;
                if let Some(trace) = self.traces.get_mut(&trace_id) {
                    vec_insert(&mut trace.metrics, name, val);
                }
            }
            OpCode::SetTraceOrigin => {
                let origin = self.get_string_arg(index)?;
                let trace_id = self.get_span(op.slot_index)?.trace_id;
                if let Some(trace) = self.traces.get_mut(&trace_id) {
                    trace.origin = Some(origin);
                }
            }
            OpCode::CreateSpan => {
                let span_id: u64 = self.change_buffer.read(index)?;
                let trace_id: u128 = self.change_buffer.read(index)?;
                let parent_id: u64 = self.get_num_arg(index)?;
                let name = self.get_string_arg(index)?;
                let start: i64 = self.get_num_arg(index)?;
                let mut span =
                    new_span_pooled(&mut self.span_pool, span_id, parent_id, trace_id);
                span.name = name;
                span.start = start;
                self.apply_default_meta(&mut span);
                self.ensure_slot(op.slot_index);
                self.spans[op.slot_index as usize] = Some(span);
                self.deferred_meta[op.slot_index as usize].clear();
                self.deferred_metrics[op.slot_index as usize].clear();
                self.traces.get_or_insert_default(trace_id).span_count += 1;
            }
            OpCode::CreateSpanFull => {
                let span_id: u64 = self.change_buffer.read(index)?;
                let trace_id: u128 = self.change_buffer.read(index)?;
                let parent_id: u64 = self.get_num_arg(index)?;
                let name = self.get_string_arg(index)?;
                let service = self.get_string_arg(index)?;
                let resource = self.get_string_arg(index)?;
                let r#type = self.get_string_arg(index)?;
                let start: i64 = self.get_num_arg(index)?;
                let mut span =
                    new_span_pooled(&mut self.span_pool, span_id, parent_id, trace_id);
                span.name = name;
                span.service = service;
                span.resource = resource;
                span.r#type = r#type;
                span.start = start;
                self.apply_default_meta(&mut span);
                self.ensure_slot(op.slot_index);
                self.spans[op.slot_index as usize] = Some(span);
                self.deferred_meta[op.slot_index as usize].clear();
                self.deferred_metrics[op.slot_index as usize].clear();
                self.traces.get_or_insert_default(trace_id).span_count += 1;
            }
            OpCode::BatchSetMeta => {
                let count: u32 = self.get_num_arg(index)?;
                let idx = op.slot_index as usize;
                for _ in 0..count {
                    let key_id: u32 = self.get_num_arg(index)?;
                    let val_id: u32 = self.get_num_arg(index)?;
                    if idx < self.deferred_meta.len() {
                        deferred_meta_insert(&mut self.deferred_meta[idx], key_id, val_id);
                    }
                }
            }
            OpCode::BatchSetMetric => {
                let count: u32 = self.get_num_arg(index)?;
                let idx = op.slot_index as usize;
                for _ in 0..count {
                    let key_id: u32 = self.get_num_arg(index)?;
                    let val: f64 = self.get_num_arg(index)?;
                    if idx < self.deferred_metrics.len() {
                        deferred_metric_insert(&mut self.deferred_metrics[idx], key_id, val);
                    }
                }
            }
        };

        Ok(())
    }

    pub fn string_table_insert_one(&mut self, key: u32, val: T::Text) {
        let idx = key as usize;
        if idx >= self.string_table.len() {
            self.string_table.resize_with(idx + 1, || None);
        }
        self.string_table[idx] = Some(val);
    }

    pub fn string_table_evict_one(&mut self, key: u32) {
        let idx = key as usize;
        if idx < self.string_table.len() {
            self.string_table[idx] = None;
        }
    }
}
