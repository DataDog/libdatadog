use std::collections::HashMap;

use anyhow::{anyhow, Result};

mod utils;
use utils::*;

mod trace;
use trace::*;

use crate::span::{Span, SpanText};

#[derive(Clone, Copy)]
pub struct ChangeBuffer(*const u8);
unsafe impl Send for ChangeBuffer {}
unsafe impl Sync for ChangeBuffer {}

impl std::ops::Deref for ChangeBuffer {
    type Target = *const u8;
    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

pub struct ChangeBufferState<T: SpanText + Clone> {
    change_buffer: ChangeBuffer,
    spans: HashMap<u64, Span<T>>,
    traces: HashMap<u128, Trace<T>>,
    string_table: HashMap<u32, T>,
    tracer_service: T,
    tracer_language: T,
    pid: f64,
}

fn new_span<T: SpanText>(span_id: u64, parent_id: u64, trace_id: u128) -> Span<T> {
    Span {
        span_id,
        trace_id,
        parent_id,
        ..Default::default()
    }
}

impl<T: SpanText + Clone> ChangeBufferState<T> {
    pub fn new(change_buffer: ChangeBuffer, tracer_service: T, tracer_language: T, pid: f64) -> Self {
        ChangeBufferState {
            change_buffer,
            spans: HashMap::new(),
            traces: HashMap::new(),
            string_table: HashMap::new(),
            tracer_service,
            tracer_language,
            pid,
        }
    }

    pub fn flush_chunk(&mut self, span_ids: Vec<u64>, first_is_local_root:bool) -> Result<Vec<Span<T>>> {
        let mut chunk_trace_id: Option<u128> = None;
        let mut is_local_root = first_is_local_root;
        let mut is_chunk_root = true;

        let spans_vec = span_ids.iter().map(|span_id| -> Result<Span<T>> {
            let maybe_span = self.spans.remove(span_id);

            let mut span = maybe_span.ok_or_else(|| anyhow!("span not found: {}", span_id))?;
            chunk_trace_id = Some(span.trace_id);

            if is_local_root {
                self.copy_in_sampling_tags(&mut span);
                span.metrics.insert(T::from_static_str("_dd.top_level"), 1.0);
                is_local_root = false;
            }
            if is_chunk_root {
                self.copy_in_chunk_tags(&mut span);
                is_chunk_root = false;
            }

            self.process_one_span(&mut span);

            Ok(span)
        }).collect::<Result<Vec<_>>>()?;

        Ok(spans_vec)
    }

    fn copy_in_sampling_tags(&self, span: &mut Span<T>) {
        if let Some(trace) = self.traces.get(&span.trace_id) {
            if let Some(rule) = trace.sampling_rule_decision {
                span.metrics.insert(T::from_static_str("_dd.rule_psr"), rule);
            }
            if let Some(rule) = trace.sampling_limit_decision {
                span.metrics.insert(T::from_static_str("_dd.;_psr"), rule);
            }
            if let Some(rule) = trace.sampling_agent_decision {
                span.metrics.insert(T::from_static_str("_dd.agent_psr"), rule);
            }

        }
    }

    fn copy_in_chunk_tags(&self, span: &mut Span<T>) {
        if let Some(trace) = self.traces.get(&span.trace_id) {
            span.meta.reserve(trace.meta.len());
            span.meta.extend(trace.meta.clone());
            span.metrics.reserve(trace.metrics.len());
            span.metrics.extend(trace.metrics.clone());
        }
    }

    fn process_one_span(&self, span: &mut Span<T>) {
        // TODO span.sample();
        
        if let Some(kind) = span.meta.get("kind") {
            if kind != &T::from_static_str("internal") {
                span.metrics.insert(T::from_static_str("_dd.measured"), 1.0);
            }
        }

        if span.service != self.tracer_service {
            span.meta.insert(T::from_static_str("_dd.base_service"), self.tracer_service.clone());
            // TODO span.service should be added to the "extra services" used by RC, which is not
            // yet implemented here
        }

        // SKIP setting single-span ingestion. They should be set when sampling is finalized for
        // the span.

        span.meta.insert(T::from_static_str("language"), self.tracer_language.clone());
        span.metrics.insert(T::from_static_str("process_id"), self.pid);

        if let Some(trace) = self.traces.get(&span.trace_id) {
            if let Some(origin) = trace.origin.clone() {
                span.meta.insert(T::from_static_str("_dd.origin"), origin);
            }
        }

        // SKIP hostname. This can be an option to the span constructor, so we'll set the tag at
        // that point.

        // TODO Sampling priority, if we're not doing that ahead of time.
    }

    pub fn flush_change_buffer (&mut self) -> Result<()>{
        let mut index = 0;
        let buf = *self.change_buffer;
        let mut count: u64 = get_num_raw(buf, &mut index);

        while count > 0 {
            let op = BufferedOperation::from_buf(&self.change_buffer, &mut index);
            self.interpret_operation(&mut index, &op)?;
            count -= 1;
        }

        // Write 0 back to the count position so the writing side of the buffer knows the queue was
        // flushed
        let buf_mut = buf as *mut u8;
        unsafe {
            std::ptr::copy_nonoverlapping([0u8; 8].as_ptr(), buf_mut, 8);
        }

        Ok(())
    }

    fn get_string_arg(&self, index: &mut usize) -> Result<T> {
        let num: u32 = self.get_num_arg(index);
        self.string_table.get(&num).cloned().ok_or_else(|| {
            anyhow!("string not found internally: {}", num)
        })
    }

    fn get_num_arg<U: Copy + FromBytes>(&self, index: &mut usize) -> U {
        get_num_raw(*self.change_buffer, index)
    }

    fn get_mut_span(&mut self, id: &u64) -> Result<&mut Span<T>> {
        self.spans.get_mut(id).ok_or_else(|| {
            anyhow!("span not found internally: {}", id)
        })
    }

    fn get_span(&self, id: &u64) -> Result<&Span<T>> {
        self.spans.get(id).ok_or_else(|| {
            anyhow!("span not found internally: {}", id)
        })
    }

    fn interpret_operation(&mut self, index: &mut usize, op: &BufferedOperation) -> Result<()> {
        match op.opcode {
            OpCode::Create => {
                let trace_id: u128 = self.get_num_arg(index);
                let parent_id = self.get_num_arg(index);
                let span = new_span(op.span_id, parent_id, trace_id);
                self.spans.insert(op.span_id, span);
                // Ensure trace exists (creates new one if this is the first span for this trace)
                self.traces.entry(trace_id).or_default();

                // *self.trace_span_counts.entry(trace_id).or_insert(0) += 1;
            }
            OpCode::SetMetaAttr => {
                let name = self.get_string_arg(index)?;
                let val = self.get_string_arg(index)?;
                let span = self.get_mut_span(&op.span_id)?;
                span.meta.insert(name, val);
            }
            OpCode::SetMetricAttr => {
                let name = self.get_string_arg(index)?;
                let val: f64 = self.get_num_arg(index);
                let span = self.get_mut_span(&op.span_id)?;
                span.metrics.insert(name, val);
            }
            OpCode::SetServiceName => {
                self.get_mut_span(&op.span_id)?.service = self.get_string_arg(index)?;
            }
            OpCode::SetResourceName => {
                self.get_mut_span(&op.span_id)?.resource = self.get_string_arg(index)?;
            }
            OpCode::SetError => {
                self.get_mut_span(&op.span_id)?.error = self.get_num_arg(index);
            }
            OpCode::SetStart => {
                self.get_mut_span(&op.span_id)?.start = self.get_num_arg(index);
            }
            OpCode::SetDuration => {
                self.get_mut_span(&op.span_id)?.duration = self.get_num_arg(index);
            }
            OpCode::SetType => {
                self.get_mut_span(&op.span_id)?.r#type = self.get_string_arg(index)?;
            }
            OpCode::SetName => {
                self.get_mut_span(&op.span_id)?.name = self.get_string_arg(index)?;
            }
            OpCode::SetTraceMetaAttr => {
                let name = self.get_string_arg(index)?;
                let val = self.get_string_arg(index)?;
                let trace_id = self.get_span(&op.span_id)?.trace_id;
                if let Some(trace) = self.traces.get_mut(&trace_id) {
                    trace.meta.insert(name, val);
                }
            }
            OpCode::SetTraceMetricsAttr => {
                let name = self.get_string_arg(index)?;
                let val = self.get_num_arg(index);
                let trace_id = self.get_span(&op.span_id)?.trace_id;
                if let Some(trace) = self.traces.get_mut(&trace_id) {
                    trace.metrics.insert(name, val);
                }
            }
            OpCode::SetTraceOrigin => {
                let origin = self.get_string_arg(index)?;
                let trace_id = self.get_span(&op.span_id)?.trace_id;
                if let Some(trace) = self.traces.get_mut(&trace_id) {
                    trace.origin = Some(origin);
                }
            }
        };

        Ok(())
    }

    pub fn string_table_insert_one(&mut self, key: u32, val: T) {
        self.string_table.insert(key, val);
    }

    pub fn string_table_evict_one(&mut self, key: u32) {
        self.string_table.remove(&key);
    }
}

#[repr(u64)]
pub enum OpCode {
    Create = 0,
    SetMetaAttr = 1,
    SetMetricAttr = 2,
    SetServiceName = 3,
    SetResourceName = 4,
    SetError = 5,
    SetStart = 6,
    SetDuration = 7,
    SetType = 8,
    SetName = 9,
    SetTraceMetaAttr = 10,
    SetTraceMetricsAttr = 11,
    SetTraceOrigin = 12,
    // TODO: SpanLinks, SpanEvents, StructAttr
}

impl From<u64> for OpCode {
    fn from(val: u64) -> Self {
        unsafe { std::mem::transmute(val) }
    }
}

pub struct BufferedOperation {
    pub opcode: OpCode,
    pub span_id: u64,
}

impl BufferedOperation {
    pub fn from_buf(buf: &ChangeBuffer, index: &mut usize) -> Self {
        BufferedOperation {
            opcode: get_num_raw::<u64>(**buf, index).into(),
            span_id: get_num_raw(**buf, index),
        }
    }
}
