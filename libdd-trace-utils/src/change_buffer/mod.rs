/// Errors that can occur when operating on a [`ChangeBuffer`] or [`ChangeBufferState`].
#[derive(Debug)]
pub enum ChangeBufferError {
    SpanNotFound(u64),
    StringNotFound(u32),
    ReadOutOfBounds { offset: usize, len: usize },
    WriteOutOfBounds { offset: usize, len: usize },
    UnknownOpcode(u32),
}

impl std::fmt::Display for ChangeBufferError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ChangeBufferError::SpanNotFound(id) => write!(f, "span not found: {id}"),
            ChangeBufferError::StringNotFound(id) => {
                write!(f, "string not found internally: {id}")
            }
            ChangeBufferError::ReadOutOfBounds { offset, len } => {
                write!(f, "read out of bounds: offset={offset}, len={len}")
            }
            ChangeBufferError::WriteOutOfBounds { offset, len } => {
                write!(f, "write out of bounds: offset={offset}, len={len}")
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
        eprintln!("[libdatadog pipeline] experiment: p1 (commit: {})", env!("GIT_COMMIT"));
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

    /// Diagnostic: number of spans currently in the Vec.
    pub fn spans_count(&self) -> usize {
        self.spans.iter().filter(|s| s.is_some()).count()
    }

    /// Diagnostic: string table length.
    pub fn string_table_len(&self) -> usize {
        self.string_table.len()
    }

    /// Diagnostic: span pool size.
    pub fn span_pool_len(&self) -> usize {
        self.span_pool.len()
    }

    /// Return flushed spans to the pool for reuse. Call this after
    /// serialization/send is complete and the spans are no longer needed.
    pub fn recycle_spans(&mut self, spans: Vec<Span<T>>) {
        // Cap the pool to avoid unbounded growth (128 spans ~ 2-3x typical concurrency)
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

            // Materialize any deferred tags for this slot
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

        // Clean up traces if no spans remain. Track all distinct trace IDs
        // in the chunk to handle chunks spanning multiple traces.
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
        // TODO span.sample();

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
            // TODO span.service should be added to the "extra services" used by RC, which is not
            // yet implemented here
        }

        // SKIP setting single-span ingestion. They should be set when sampling is finalized for
        // the span.

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

        // SKIP hostname. This can be an option to the span constructor, so we'll set the tag at
        // that point.

        // TODO Sampling priority, if we're not doing that ahead of time.
    }

    pub fn flush_change_buffer(&mut self) -> Result<()> {
        let mut index = 0;
        // Count is written as u64 by JS (two u32 writes at offset 0 and 4).
        // Read as u64 to consume all 8 bytes, keeping alignment with the ops
        // that start at offset 8. Only the low 32 bits carry the count value.
        let mut count = self.change_buffer.read::<u64>(&mut index)? as u32;

        // Cache the last slot to skip redundant lookups when consecutive
        // operations target the same span (the common case).
        let mut cached_slot: u32 = u32::MAX;
        let mut cached_span_ptr: *mut Span<T> = std::ptr::null_mut();
        let mut cached_deferred_meta: *mut Vec<(u32, u32)> = std::ptr::null_mut();
        let mut cached_deferred_metrics: *mut Vec<(u32, f64)> = std::ptr::null_mut();

        while count > 0 {
            let op = BufferedOperation::from_buf(&self.change_buffer, &mut index)?;

            // For operations that need a mutable span reference, try the cache
            // first.
            match op.opcode {
                OpCode::Create | OpCode::CreateSpan | OpCode::CreateSpanFull => {
                    // These insert into self.spans, invalidating any cached
                    // pointer
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

        // Zero the full u64 count field (JS reads/writes both u32 words)
        self.change_buffer.write_u32(0, 0)?;
        self.change_buffer.write_u32(4, 0)?;

        Ok(())
    }

    /// Like interpret_operation, but uses a cached span pointer to avoid
    /// redundant Vec lookups for consecutive operations on the same span.
    fn interpret_operation_cached(
        &mut self,
        index: &mut usize,
        op: &BufferedOperation,
        cached_slot: &mut u32,
        cached_span_ptr: &mut *mut Span<T>,
        cached_deferred_meta: &mut *mut Vec<(u32, u32)>,
        cached_deferred_metrics: &mut *mut Vec<(u32, f64)>,
    ) -> Result<()> {
        // Try to reuse the cached span pointer
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
            // Update deferred caches
            *cached_deferred_meta = &mut self.deferred_meta[slot] as *mut Vec<(u32, u32)>;
            *cached_deferred_metrics = &mut self.deferred_metrics[slot] as *mut Vec<(u32, f64)>;
            span
        };

        // SAFETY: span_ptr is valid — it was obtained from self.spans above
        // or from the cache which was set in a previous iteration of the same loop.
        // self.spans is not modified during this function (no inserts/removes).
        // The only shared references are to self.string_table and self.change_buffer
        // (read-only), which don't alias with self.spans.
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
            // Create variants are handled in the caller, never reach here
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

    /// Allocate a span header slot. Returns the index into span_headers.
    /// JS uses this index * SPAN_HEADER_SIZE + base_ptr to create a DataView.
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

    /// Materialize a SpanHeader into a full Span in the spans Vec.
    /// Called during flush_chunk to convert the header fields + string table
    /// IDs into a complete Span with resolved strings. Also registers the
    /// span in the trace map.
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

        // Resolve string table IDs
        if h.name_id > 0 || true {
            if let Some(name) = self.get_string(h.name_id) {
                span.name = name;
            }
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

        // Apply default meta tags
        self.apply_default_meta(&mut span);

        // If there's already a span in the Vec slot (from change buffer meta/metrics
        // writes), merge the header fields into it. Otherwise insert new.
        // For now, use span_id-based lookup across the Vec
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
            // meta/metrics already populated by change buffer ops
            for (k, v) in &self.default_meta {
                vec_insert(&mut existing.meta, k.clone(), v.clone());
            }
        } else {
            // Find a free slot or push
            let slot_idx = self.spans.len();
            self.spans.push(Some(span));
            // Ensure deferred vecs are sized
            if slot_idx >= self.deferred_meta.len() {
                self.deferred_meta.resize_with(slot_idx + 1, Vec::new);
                self.deferred_metrics.resize_with(slot_idx + 1, Vec::new);
            }
        }

        self.traces.get_or_insert_default(trace_id).span_count += 1;

        // Free the header slot
        self.span_headers[header_idx as usize].active = 0;
        self.header_free_list.push(header_idx);

        Ok(span_id)
    }

    /// Get a mutable reference to a span by slot.
    pub fn span_mut(&mut self, slot: &u32) -> Result<&mut Span<T>> {
        self.spans
            .get_mut(*slot as usize)
            .and_then(|opt| opt.as_mut())
            .ok_or(ChangeBufferError::SpanNotFound(*slot as u64))
    }

    /// Look up a string by ID, returning a clone.
    pub fn get_string(&self, id: u32) -> Option<T::Text> {
        self.string_table
            .get(id as usize)
            .and_then(|opt| opt.clone())
    }

    /// Set default meta tags that are automatically applied to every new span.
    /// Call this once at init time with the config tags (service, version,
    /// runtime-id, etc.).
    pub fn set_default_meta(&mut self, tags: Vec<(T::Text, T::Text)>) {
        self.default_meta = tags;
    }

    /// Apply default meta tags to a span.
    fn apply_default_meta(&self, span: &mut Span<T>) {
        for (key, value) in &self.default_meta {
            vec_insert(&mut span.meta, key.clone(), value.clone());
        }
    }

    /// Materialize deferred tags for a slot into the given span.
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

    /// Materialize deferred tags for a slot in-place. For WASM getters that
    /// need to read span meta/metrics before flush.
    pub fn materialize_slot(&mut self, slot: u32) {
        let idx = slot as usize;
        // Collect resolved pairs first to avoid borrow conflict with self.spans
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
                // Combined Create + SetName + SetStart
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
                // Combined Create + SetName + SetService + SetResource + SetType + SetStart
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::span::SliceData;

    /// Helper to build the binary buffer layout that flush_change_buffer expects.
    /// Layout: [count: u32][operations...]
    trait ToLeBytes {
        fn extend_le_bytes(&self, buf: &mut Vec<u8>);
    }

    macro_rules! impl_to_le_bytes {
        ($($ty:ty),*) => {
            $(impl ToLeBytes for $ty {
                fn extend_le_bytes(&self, buf: &mut Vec<u8>) {
                    buf.extend_from_slice(&self.to_le_bytes());
                }
            })*
        };
    }

    impl_to_le_bytes!(u32, u64, u128, i32, i64, f64);

    struct BufBuilder {
        data: Vec<u8>,
        op_count: u32,
    }

    impl BufBuilder {
        fn new() -> Self {
            // 8 bytes for the count field (u64: low u32 is count, high u32 is 0)
            Self {
                data: vec![0u8; 8],
                op_count: 0,
            }
        }

        fn push<T: ToLeBytes>(&mut self, val: T) {
            val.extend_le_bytes(&mut self.data);
        }

        fn push_op_header(&mut self, opcode: OpCode, slot: u32) {
            // Opcode is written as u64 (low u32 = opcode, high u32 = 0),
            // matching the JS encoding.
            self.push(opcode as u32);
            self.push(0u32);
            self.push(slot);
            self.op_count += 1;
        }

        /// Write a Create operation: op_header(slot) + span_id + trace_id + parent_id
        fn push_create(&mut self, slot: u32, span_id: u64, trace_id: u128, parent_id: u64) {
            self.push_op_header(OpCode::Create, slot);
            self.push(span_id);
            self.push(trace_id);
            self.push(parent_id);
        }

        fn finalize(&mut self) -> ChangeBuffer {
            // Write count as u64 LE (low u32 = count, high u32 = 0)
            self.data[0..4].copy_from_slice(&self.op_count.to_le_bytes());
            self.data[4..8].copy_from_slice(&0u32.to_le_bytes());
            unsafe { ChangeBuffer::from_raw_parts(self.data.as_mut_ptr(), self.data.len()) }
        }
    }

    fn make_state(buf: ChangeBuffer) -> ChangeBufferState<SliceData<'static>> {
        ChangeBufferState::new(buf, "my-service", "rust", 1234)
    }

    fn create_span_directly(
        state: &mut ChangeBufferState<SliceData<'static>>,
        slot: u32,
        span_id: u64,
        trace_id: u128,
        parent_id: u64,
    ) {
        let span = Span {
            span_id,
            trace_id,
            parent_id,
            meta: Vec::with_capacity(8),
            metrics: Vec::with_capacity(4),
            ..Default::default()
        };
        let idx = slot as usize;
        if idx >= state.spans.len() {
            state.spans.resize_with(idx + 1, || None);
        }
        if idx >= state.deferred_meta.len() {
            state.deferred_meta.resize_with(idx + 1, Vec::new);
            state.deferred_metrics.resize_with(idx + 1, Vec::new);
        }
        state.spans[idx] = Some(span);
        state.deferred_meta[idx].clear();
        state.deferred_metrics[idx].clear();
        state.traces.get_or_insert_default(trace_id).span_count += 1;
    }

    // -- string table --

    #[test]
    fn string_table_insert_and_evict() {
        let mut builder = BufBuilder::new();
        let buf = builder.finalize();
        let mut state = make_state(buf);

        assert!(state.string_table.is_empty());

        state.string_table_insert_one(1, "hello");
        assert_eq!(state.string_table.get(1), Some(&Some("hello")));

        state.string_table_insert_one(2, "world");
        assert_eq!(state.string_table.get(1), Some(&Some("hello")));
        assert_eq!(state.string_table.get(2), Some(&Some("world")));

        state.string_table_evict_one(1);
        assert_eq!(state.string_table.get(1), Some(&None));
        assert_eq!(state.string_table.get(2), Some(&Some("world")));

        state.string_table_evict_one(2);
        assert_eq!(state.string_table.get(2), Some(&None));
    }

    // -- get_span / get_trace --

    #[test]
    fn get_span_missing_returns_error() {
        let mut builder = BufBuilder::new();
        let buf = builder.finalize();
        let state = make_state(buf);
        assert!(state.get_span(0).is_err());
    }

    #[test]
    fn get_trace_missing_returns_none() {
        let mut builder = BufBuilder::new();
        let buf = builder.finalize();
        let state = make_state(buf);
        assert!(state.get_trace(&42).is_none());
    }

    // -- flush_change_buffer: Create --

    #[test]
    fn flush_create_inserts_span_and_trace() -> Result<()> {
        let mut builder = BufBuilder::new();
        builder.push_create(0, 100, 200, 50);
        let buf = builder.finalize();
        let mut state = make_state(buf);

        state.flush_change_buffer()?;

        let span = state.get_span(0)?;
        assert_eq!(span.span_id, 100);
        assert_eq!(span.trace_id, 200);
        assert_eq!(span.parent_id, 50);

        assert!(state.get_trace(&200).is_some());
        assert_eq!(state.get_trace(&200).unwrap().span_count, 1);
        Ok(())
    }

    #[test]
    fn flush_create_multiple_spans_same_trace() -> Result<()> {
        let mut builder = BufBuilder::new();
        builder.push_create(0, 1, 100, 0);
        builder.push_create(1, 2, 100, 1);
        builder.push_create(2, 3, 100, 1);
        let buf = builder.finalize();
        let mut state = make_state(buf);

        state.flush_change_buffer()?;

        assert_eq!(state.get_trace(&100).unwrap().span_count, 3);
        assert!(state.get_span(0).is_ok());
        assert!(state.get_span(1).is_ok());
        assert!(state.get_span(2).is_ok());
        Ok(())
    }

    // -- flush_change_buffer: Set* operations --

    #[test]
    fn flush_set_meta_attr() -> Result<()> {
        let mut builder = BufBuilder::new();
        builder.push_create(0, 1, 100, 0);
        builder.push_op_header(OpCode::SetMetaAttr, 0);
        builder.push(10u32); // string table key for name
        builder.push(11u32); // string table key for value
        let buf = builder.finalize();

        let mut state = make_state(buf);
        state.string_table_insert_one(10, "http.method");
        state.string_table_insert_one(11, "GET");

        state.flush_change_buffer()?;

        // Meta is deferred — materialize before reading
        state.materialize_slot(0);
        let span = state.get_span(0)?;
        assert_eq!(vec_get(&span.meta, &"http.method"), Some(&"GET"));
        Ok(())
    }

    #[test]
    fn flush_set_metric_attr() -> Result<()> {
        let mut builder = BufBuilder::new();
        builder.push_create(0, 1, 100, 0);
        builder.push_op_header(OpCode::SetMetricAttr, 0);
        builder.push(10u32); // string table key for name
        builder.push(99.5f64);
        let buf = builder.finalize();

        let mut state = make_state(buf);
        state.string_table_insert_one(10, "my.metric");

        state.flush_change_buffer()?;

        // Metrics are deferred — materialize before reading
        state.materialize_slot(0);
        let span = state.get_span(0)?;
        assert_eq!(vec_get(&span.metrics, &"my.metric"), Some(&99.5));
        Ok(())
    }

    #[test]
    fn flush_set_service_name() -> Result<()> {
        let mut builder = BufBuilder::new();
        builder.push_create(0, 1, 100, 0);
        builder.push_op_header(OpCode::SetServiceName, 0);
        builder.push(10u32);
        let buf = builder.finalize();

        let mut state = make_state(buf);
        state.string_table_insert_one(10, "web-server");

        state.flush_change_buffer()?;
        assert_eq!(state.get_span(0)?.service, "web-server");
        Ok(())
    }

    #[test]
    fn flush_set_resource_name() -> Result<()> {
        let mut builder = BufBuilder::new();
        builder.push_create(0, 1, 100, 0);
        builder.push_op_header(OpCode::SetResourceName, 0);
        builder.push(10u32);
        let buf = builder.finalize();

        let mut state = make_state(buf);
        state.string_table_insert_one(10, "GET /api/users");

        state.flush_change_buffer()?;
        assert_eq!(state.get_span(0)?.resource, "GET /api/users");
        Ok(())
    }

    #[test]
    fn flush_set_error() -> Result<()> {
        let mut builder = BufBuilder::new();
        builder.push_create(0, 1, 100, 0);
        builder.push_op_header(OpCode::SetError, 0);
        builder.push(1i32);
        let buf = builder.finalize();

        let mut state = make_state(buf);
        state.flush_change_buffer()?;
        assert_eq!(state.get_span(0)?.error, 1);
        Ok(())
    }

    #[test]
    fn flush_set_start_and_duration() -> Result<()> {
        let mut builder = BufBuilder::new();
        builder.push_create(0, 1, 100, 0);
        builder.push_op_header(OpCode::SetStart, 0);
        builder.push(1_000_000i64);
        builder.push_op_header(OpCode::SetDuration, 0);
        builder.push(500i64);
        let buf = builder.finalize();

        let mut state = make_state(buf);
        state.flush_change_buffer()?;

        let span = state.get_span(0)?;
        assert_eq!(span.start, 1_000_000);
        assert_eq!(span.duration, 500);
        Ok(())
    }

    #[test]
    fn flush_set_type_and_name() -> Result<()> {
        let mut builder = BufBuilder::new();
        builder.push_create(0, 1, 100, 0);
        builder.push_op_header(OpCode::SetType, 0);
        builder.push(10u32);
        builder.push_op_header(OpCode::SetName, 0);
        builder.push(11u32);
        let buf = builder.finalize();

        let mut state = make_state(buf);
        state.string_table_insert_one(10, "web");
        state.string_table_insert_one(11, "http.request");

        state.flush_change_buffer()?;

        let span = state.get_span(0)?;
        assert_eq!(span.r#type, "web");
        assert_eq!(span.name, "http.request");
        Ok(())
    }

    // -- flush_change_buffer: trace-level operations --

    #[test]
    fn flush_set_trace_meta_attr() -> Result<()> {
        let mut builder = BufBuilder::new();
        builder.push_create(0, 1, 100, 0);
        builder.push_op_header(OpCode::SetTraceMetaAttr, 0);
        builder.push(10u32);
        builder.push(11u32);
        let buf = builder.finalize();

        let mut state = make_state(buf);
        state.string_table_insert_one(10, "env");
        state.string_table_insert_one(11, "production");

        state.flush_change_buffer()?;

        let trace = state.get_trace(&100).unwrap();
        assert_eq!(vec_get(&trace.meta, &"env"), Some(&"production"));
        Ok(())
    }

    #[test]
    fn flush_set_trace_metrics_attr() -> Result<()> {
        let mut builder = BufBuilder::new();
        builder.push_create(0, 1, 100, 0);
        builder.push_op_header(OpCode::SetTraceMetricsAttr, 0);
        builder.push(10u32);
        builder.push(0.75f64);
        let buf = builder.finalize();

        let mut state = make_state(buf);
        state.string_table_insert_one(10, "_sampling_priority_v1");

        state.flush_change_buffer()?;

        let trace = state.get_trace(&100).unwrap();
        assert_eq!(
            vec_get(&trace.metrics, &"_sampling_priority_v1"),
            Some(&0.75)
        );
        Ok(())
    }

    #[test]
    fn flush_set_trace_origin() -> Result<()> {
        let mut builder = BufBuilder::new();
        builder.push_create(0, 1, 100, 0);
        builder.push_op_header(OpCode::SetTraceOrigin, 0);
        builder.push(10u32);
        let buf = builder.finalize();

        let mut state = make_state(buf);
        state.string_table_insert_one(10, "synthetics");

        state.flush_change_buffer()?;

        let trace = state.get_trace(&100).unwrap();
        assert_eq!(trace.origin, Some("synthetics"));
        Ok(())
    }

    // -- flush_change_buffer resets count --

    #[test]
    fn flush_change_buffer_resets_count_to_zero() -> Result<()> {
        let mut builder = BufBuilder::new();
        builder.push_create(0, 1, 100, 0);
        let buf = builder.finalize();
        let mut state = make_state(buf);

        state.flush_change_buffer()?;

        // The count at offset 0 should now be 0
        let mut index = 0;
        let count = state.change_buffer.read::<u64>(&mut index)?;
        assert_eq!(count, 0);
        Ok(())
    }

    // -- flush_change_buffer with zero count --

    #[test]
    fn flush_change_buffer_empty_is_noop() -> Result<()> {
        let mut builder = BufBuilder::new();
        // No operations pushed, count stays 0
        let buf = builder.finalize();
        let mut state = make_state(buf);

        state.flush_change_buffer()?;
        assert_eq!(state.spans_count(), 0);
        assert!(state.traces.is_empty());
        Ok(())
    }

    // -- flush_chunk --

    #[test]
    fn flush_chunk_returns_spans_and_removes_from_state() -> Result<()> {
        let mut builder = BufBuilder::new();
        let buf = builder.finalize();
        let mut state = make_state(buf);

        create_span_directly(&mut state, 0, 1, 100, 0);
        create_span_directly(&mut state, 1, 2, 100, 1);

        let spans = state.flush_chunk(vec![0, 1], false)?;
        assert_eq!(spans.len(), 2);
        assert_eq!(spans[0].span_id, 1);
        assert_eq!(spans[1].span_id, 2);

        // Spans removed from state
        assert!(state.get_span(0).is_err());
        assert!(state.get_span(1).is_err());
        Ok(())
    }

    #[test]
    fn flush_chunk_missing_span_returns_error() {
        let mut builder = BufBuilder::new();
        let buf = builder.finalize();
        let mut state = make_state(buf);

        assert!(state.flush_chunk(vec![0], false).is_err());
    }

    #[test]
    fn flush_chunk_local_root_gets_top_level_tag() -> Result<()> {
        let mut builder = BufBuilder::new();
        let buf = builder.finalize();
        let mut state = make_state(buf);

        create_span_directly(&mut state, 0, 1, 100, 0);
        create_span_directly(&mut state, 1, 2, 100, 1);

        let spans = state.flush_chunk(vec![0, 1], true)?;

        // First span (local root) gets _dd.top_level
        assert_eq!(vec_get(&spans[0].metrics, &"_dd.top_level"), Some(&1.0));
        // Second span does not
        assert_eq!(vec_get(&spans[1].metrics, &"_dd.top_level"), None);
        Ok(())
    }

    #[test]
    fn flush_chunk_local_root_gets_sampling_tags() -> Result<()> {
        let mut builder = BufBuilder::new();
        let buf = builder.finalize();
        let mut state = make_state(buf);

        create_span_directly(&mut state, 0, 1, 100, 0);

        // Set sampling decisions on the trace
        let trace = state.traces.get_mut(&100).unwrap();
        trace.sampling_rule_decision = Some(0.5);
        trace.sampling_limit_decision = Some(0.8);
        trace.sampling_agent_decision = Some(1.0);

        let spans = state.flush_chunk(vec![0], true)?;

        assert_eq!(vec_get(&spans[0].metrics, &"_dd.rule_psr"), Some(&0.5));
        assert_eq!(vec_get(&spans[0].metrics, &"_dd.limit_psr"), Some(&0.8));
        assert_eq!(vec_get(&spans[0].metrics, &"_dd.agent_psr"), Some(&1.0));
        Ok(())
    }

    #[test]
    fn flush_chunk_chunk_root_gets_trace_tags() -> Result<()> {
        let mut builder = BufBuilder::new();
        let buf = builder.finalize();
        let mut state = make_state(buf);

        create_span_directly(&mut state, 0, 1, 100, 0);
        create_span_directly(&mut state, 1, 2, 100, 1);

        // Set trace-level meta and metrics
        let trace = state.traces.get_mut(&100).unwrap();
        vec_insert(&mut trace.meta, "env", "staging");
        vec_insert(&mut trace.metrics, "_sampling_priority_v1", 2.0);

        let spans = state.flush_chunk(vec![0, 1], false)?;

        // First span (chunk root) gets trace tags
        assert_eq!(vec_get(&spans[0].meta, &"env"), Some(&"staging"));
        assert_eq!(
            vec_get(&spans[0].metrics, &"_sampling_priority_v1"),
            Some(&2.0)
        );
        // Second span does not get trace-level tags
        assert_eq!(vec_get(&spans[1].meta, &"env"), None);
        assert_eq!(
            vec_get(&spans[1].metrics, &"_sampling_priority_v1"),
            None
        );
        Ok(())
    }

    // -- process_one_span behaviors (tested via flush_chunk) --

    #[test]
    fn flush_chunk_sets_language_and_pid() -> Result<()> {
        let mut builder = BufBuilder::new();
        let buf = builder.finalize();
        let mut state = make_state(buf);

        create_span_directly(&mut state, 0, 1, 100, 0);

        let spans = state.flush_chunk(vec![0], false)?;
        assert_eq!(vec_get(&spans[0].meta, &"language"), Some(&"rust"));
        assert_eq!(vec_get(&spans[0].metrics, &"process_id"), Some(&1234.0));
        Ok(())
    }

    #[test]
    fn flush_chunk_sets_origin_from_trace() -> Result<()> {
        let mut builder = BufBuilder::new();
        let buf = builder.finalize();
        let mut state = make_state(buf);

        create_span_directly(&mut state, 0, 1, 100, 0);
        state.traces.get_mut(&100).unwrap().origin = Some("synthetics");

        let spans = state.flush_chunk(vec![0], false)?;
        assert_eq!(
            vec_get(&spans[0].meta, &"_dd.origin"),
            Some(&"synthetics")
        );
        Ok(())
    }

    #[test]
    fn flush_chunk_sets_measured_for_non_internal_kind() -> Result<()> {
        let mut builder = BufBuilder::new();
        let buf = builder.finalize();
        let mut state = make_state(buf);

        create_span_directly(&mut state, 0, 1, 100, 0);
        vec_insert(
            &mut state.spans[0].as_mut().unwrap().meta,
            "kind",
            "client",
        );

        let spans = state.flush_chunk(vec![0], false)?;
        assert_eq!(vec_get(&spans[0].metrics, &"_dd.measured"), Some(&1.0));
        Ok(())
    }

    #[test]
    fn flush_chunk_does_not_set_measured_for_internal_kind() -> Result<()> {
        let mut builder = BufBuilder::new();
        let buf = builder.finalize();
        let mut state = make_state(buf);

        create_span_directly(&mut state, 0, 1, 100, 0);
        vec_insert(
            &mut state.spans[0].as_mut().unwrap().meta,
            "kind",
            "internal",
        );

        let spans = state.flush_chunk(vec![0], false)?;
        assert_eq!(vec_get(&spans[0].metrics, &"_dd.measured"), None);
        Ok(())
    }

    #[test]
    fn flush_chunk_sets_base_service_when_service_differs() -> Result<()> {
        let mut builder = BufBuilder::new();
        let buf = builder.finalize();
        let mut state = make_state(buf);

        create_span_directly(&mut state, 0, 1, 100, 0);
        state.spans[0].as_mut().unwrap().service = "other-service";

        let spans = state.flush_chunk(vec![0], false)?;
        assert_eq!(
            vec_get(&spans[0].meta, &"_dd.base_service"),
            Some(&"my-service")
        );
        Ok(())
    }

    #[test]
    fn flush_chunk_no_base_service_when_service_matches() -> Result<()> {
        let mut builder = BufBuilder::new();
        let buf = builder.finalize();
        let mut state = make_state(buf);

        create_span_directly(&mut state, 0, 1, 100, 0);
        state.spans[0].as_mut().unwrap().service = "my-service";

        let spans = state.flush_chunk(vec![0], false)?;
        assert_eq!(vec_get(&spans[0].meta, &"_dd.base_service"), None);
        Ok(())
    }

    // -- flush_chunk trace cleanup --

    #[test]
    fn flush_chunk_cleans_up_trace_when_all_spans_flushed() -> Result<()> {
        let mut builder = BufBuilder::new();
        let buf = builder.finalize();
        let mut state = make_state(buf);

        create_span_directly(&mut state, 0, 1, 100, 0);
        create_span_directly(&mut state, 1, 2, 100, 1);

        state.flush_chunk(vec![0, 1], false)?;

        assert!(state.get_trace(&100).is_none());
        Ok(())
    }

    #[test]
    fn flush_chunk_keeps_trace_when_spans_remain() -> Result<()> {
        let mut builder = BufBuilder::new();
        let buf = builder.finalize();
        let mut state = make_state(buf);

        create_span_directly(&mut state, 0, 1, 100, 0);
        create_span_directly(&mut state, 1, 2, 100, 1);
        create_span_directly(&mut state, 2, 3, 100, 1);

        // Flush only 2 of 3 spans
        state.flush_chunk(vec![0, 1], false)?;

        assert!(state.get_trace(&100).is_some());
        assert_eq!(state.get_trace(&100).unwrap().span_count, 1);
        Ok(())
    }
}
