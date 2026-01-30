use std::collections::HashMap;

use anyhow::{anyhow, bail, Result};

mod utils;
use utils::*;

mod trace;
pub use trace::*;

use crate::span::{Span, SpanText};

#[derive(Clone, Copy)]
pub struct ChangeBuffer {                                                                                                                                                                                                       
    ptr: *mut u8,                                                                                                                                                                                                               
    len: usize,                                                                                                                                                                                                                 
}

impl ChangeBuffer {                                                                                                                                                                                                                                                                                                                                                                      
    pub unsafe fn from_raw_parts(ptr: *const u8, len: usize) -> Self {                                                                                                                                                          
        Self { ptr: ptr as *mut u8, len }                                                                                                                                                                                       
    }                                                                                                                                                                                                                           

    fn as_slice(&self) -> &[u8] {                                                                                                                                                                                               
        unsafe { std::slice::from_raw_parts(self.ptr, self.len) }                                                                                                                                                               
    }                                                                                                                                                                                                                           

    fn as_mut_slice(&mut self) -> &mut [u8] {                                                                                                                                                                                   
        unsafe { std::slice::from_raw_parts_mut(self.ptr, self.len) }                                                                                                                                                           
    }                                                                                                                                                                                                                           

    pub fn read<T: Copy + FromBytes>(&self, index: &mut usize) -> Result<T> {
        let size = std::mem::size_of::<T>();
        let slice = self.as_slice();                                                                                                                                                                                            
        let bytes = slice.get(*index..*index + size)                                                                                                                                                                               
            .ok_or(anyhow!("read_u64 out of bounds: offset={}, len={}", *index, self.len))?;                                                                                                                                  
        let array: [u8; 8] = bytes.try_into()                                                                                                                                                                                   
            .map_err(|_| anyhow!("failed to convert slice to [u8; 8]"))?;                                                                                                                                                       
        *index += size;
        Ok(T::from_bytes(&array))     
    }

    pub fn write_u64(&mut self, offset: usize, value: u64) -> Result<()> {                                                                                                                                                      
        let len = self.len;
        let slice = self.as_mut_slice();                                                                                                                                                                                        
        let target = slice.get_mut(offset..offset + 8)                                                                                                                                                                          
            .ok_or(anyhow!("write_u64 out of bounds: offset={}, len={}", offset, len))?;                                                                                                                                 
        target.copy_from_slice(&value.to_le_bytes());                                                                                                                                                                           
        Ok(())                                                                                                                                                                                                                  
    }                                                                                                                                                                                                                           

    pub fn clear_count(&mut self) -> Result<()> {                                                                                                                                                                               
        self.write_u64(0, 0)                                                                                                                                                                                                    
            .map_err(|_| anyhow!("failed to clear buffer count"))                                                                                                                                                                            
    }
}

pub struct ChangeBufferState<T: SpanText + Clone> {
    change_buffer: ChangeBuffer,
    spans: HashMap<u64, Span<T>>,
    traces: HashMap<u128, Trace<T>>,
    string_table: HashMap<u32, T>,
    trace_span_counts: HashMap<u128, usize>,
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
    pub fn new(
        change_buffer: ChangeBuffer,
        tracer_service: T,
        tracer_language: T,
        pid: f64,
    ) -> Self {
        ChangeBufferState {
            change_buffer,
            spans: Default::default(),
            traces: Default::default(),
            string_table: Default::default(),
            trace_span_counts: Default::default(),
            tracer_service,
            tracer_language,
            pid,
        }
    }

    pub fn flush_chunk(
        &mut self,
        span_ids: Vec<u64>,
        first_is_local_root: bool,
    ) -> Result<Vec<Span<T>>> {
        let mut chunk_trace_id: Option<u128> = None;
        let mut is_local_root = first_is_local_root;
        let mut is_chunk_root = true;

        let spans_vec = span_ids
            .iter()
            .map(|span_id| -> Result<Span<T>> {
                let maybe_span = self.spans.remove(span_id);

                let mut span = maybe_span.ok_or_else(|| anyhow!("span not found: {}", span_id))?;
                chunk_trace_id = Some(span.trace_id);

                if is_local_root {
                    self.copy_in_sampling_tags(&mut span);
                    span.metrics
                        .insert(T::from_static_str("_dd.top_level"), 1.0);
                    is_local_root = false;
                }
                if is_chunk_root {
                    self.copy_in_chunk_tags(&mut span);
                    is_chunk_root = false;
                }

                self.process_one_span(&mut span);

                Ok(span)
            })
            .collect::<Result<Vec<_>>>()?;

        // Clean up trace if no spans remain for it
        if let Some(trace_id) = chunk_trace_id {
            if let Some(count) = self.trace_span_counts.get_mut(&trace_id) {
                let len = span_ids.len();
                if *count <= len {
                    // All spans for this trace have been flushed
                    self.traces.remove(&trace_id);
                    self.trace_span_counts.remove(&trace_id);
                } else {
                    *count -= len;
                }
            }
        }

        Ok(spans_vec)
    }

    fn copy_in_sampling_tags(&self, span: &mut Span<T>) {
        if let Some(trace) = self.traces.get(&span.trace_id) {
            if let Some(rule) = trace.sampling_rule_decision {
                span.metrics
                    .insert(T::from_static_str("_dd.rule_psr"), rule);
            }
            if let Some(rule) = trace.sampling_limit_decision {
                span.metrics.insert(T::from_static_str("_dd.limit_psr"), rule);
            }
            if let Some(rule) = trace.sampling_agent_decision {
                span.metrics
                    .insert(T::from_static_str("_dd.agent_psr"), rule);
            }
        }
    }

    fn copy_in_chunk_tags(&self, span: &mut Span<T>) {
        if let Some(trace) = self.traces.get(&span.trace_id) {
            span.meta.reserve(trace.meta.len());
            for (k, v) in &trace.meta {
                span.meta.insert(k.clone(), v.clone());
            }
            span.metrics.reserve(trace.metrics.len());
            for (k, v) in &trace.metrics {
                span.metrics.insert(k.clone(), *v);
            }
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
            span.meta.insert(
                T::from_static_str("_dd.base_service"),
                self.tracer_service.clone(),
            );
            // TODO span.service should be added to the "extra services" used by RC, which is not
            // yet implemented here
        }

        // SKIP setting single-span ingestion. They should be set when sampling is finalized for
        // the span.

        span.meta
            .insert(T::from_static_str("language"), self.tracer_language.clone());
        span.metrics
            .insert(T::from_static_str("process_id"), self.pid);

        if let Some(trace) = self.traces.get(&span.trace_id) {
            if let Some(origin) = trace.origin.clone() {
                span.meta.insert(T::from_static_str("_dd.origin"), origin);
            }
        }

        // SKIP hostname. This can be an option to the span constructor, so we'll set the tag at
        // that point.

        // TODO Sampling priority, if we're not doing that ahead of time.
    }

    pub fn flush_change_buffer(&mut self) -> Result<()> {
        let mut index = 0;
        let mut count = self.change_buffer.read::<u64>(&mut index)?;
        index += 8;

        while count > 0 {
            let op = BufferedOperation::from_buf(&self.change_buffer, &mut index)?;
            self.interpret_operation(&mut index, &op)?;
            count -= 1;
        }

        // Write 0 back to the count position so the writing side of the buffer knows the queue was
        // flushed
        self.change_buffer.write_u64(0, 0)?;

        Ok(())
    }

    fn get_string_arg(&self, index: &mut usize) -> Result<T> {
        let num: u32 = self.get_num_arg(index)?;
        self.string_table
            .get(&num)
            .cloned()
            .ok_or_else(|| anyhow!("string not found internally: {}", num))
    }

    fn get_num_arg<U: Copy + FromBytes>(&self, index: &mut usize) -> Result<U> {
        self.change_buffer.read(index)
    }

    fn get_mut_span(&mut self, id: &u64) -> Result<&mut Span<T>> {
        self.spans
            .get_mut(id)
            .ok_or_else(|| anyhow!("span not found internally: {}", id))
    }

    pub fn get_span(&self, id: &u64) -> Result<&Span<T>> {
        self.spans
            .get(id)
            .ok_or_else(|| anyhow!("span not found internally: {}", id))
    }

    pub fn get_trace(&self, id: &u128) -> Option<&Trace<T>> {
        self.traces.get(id)
    }

    fn interpret_operation(&mut self, index: &mut usize, op: &BufferedOperation) -> Result<()> {
        match op.opcode {
            OpCode::Create => {
                let trace_id: u128 = self.change_buffer.read(index)?;
                let parent_id = self.get_num_arg(index)?;
                let span = new_span(op.span_id, parent_id, trace_id);
                self.spans.insert(op.span_id, span);
                // Ensure trace exists (creates new one if this is the first span for this trace)
                self.traces.entry(trace_id).or_default();

                *self.trace_span_counts.entry(trace_id).or_insert(0) += 1;
            }
            OpCode::SetMetaAttr => {
                let name = self.get_string_arg(index)?;
                let val = self.get_string_arg(index)?;
                let span = self.get_mut_span(&op.span_id)?;
                span.meta.insert(name, val);
            }
            OpCode::SetMetricAttr => {
                let name = self.get_string_arg(index)?;
                let val: f64 = self.get_num_arg(index)?;
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
                self.get_mut_span(&op.span_id)?.error = self.get_num_arg(index)?;
            }
            OpCode::SetStart => {
                self.get_mut_span(&op.span_id)?.start = self.get_num_arg(index)?;
            }
            OpCode::SetDuration => {
                self.get_mut_span(&op.span_id)?.duration = self.get_num_arg(index)?;
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
                let val = self.get_num_arg(index)?;
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

impl TryFrom<u64> for OpCode {
    type Error = anyhow::Error;

    fn try_from(val: u64) -> Result<Self> {
        Ok(match val {
            0 => OpCode::Create,
            1 => OpCode::SetMetaAttr,
            2 => OpCode::SetMetricAttr,
            3 => OpCode::SetServiceName,
            4 => OpCode::SetResourceName,
            5 => OpCode::SetError,
            6 => OpCode::SetStart,
            7 => OpCode::SetDuration,
            8 => OpCode::SetType,
            9 => OpCode::SetName,
            10 => OpCode::SetTraceMetaAttr,
            11 => OpCode::SetTraceMetricsAttr,
            12 => OpCode::SetTraceOrigin,
            _ => bail!("unknown opcode")
        })
    }
}

pub struct BufferedOperation {
    pub opcode: OpCode,
    pub span_id: u64,
}

impl BufferedOperation {
    pub fn from_buf(buf: &ChangeBuffer, index: &mut usize) -> Result<Self> {
        let opcode = buf.read::<u64>(index)?.try_into()?;
        let span_id = buf.read(index)?;
        Ok(BufferedOperation {
            opcode,
            span_id,
        })
    }
}
