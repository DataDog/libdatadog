use std::collections::HashMap;

mod utils;

/// Errors that can occur when operating on a [`ChangeBuffer`] or [`ChangeBufferState`].
#[derive(Debug)]
pub enum ChangeBufferError {
    SpanNotFound(u64),
    StringNotFound(u32),
    ReadOutOfBounds { offset: usize, len: usize },
    WriteOutOfBounds { offset: usize, len: usize },
    UnknownOpcode(u64),
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
use utils::*;

mod trace;
pub use trace::*;

mod operation;
use operation::*;

mod change_buffer;
pub use change_buffer::*;

use crate::span::{Span, SpanText};

pub struct ChangeBufferState<T: SpanText + Clone> {
    change_buffer: ChangeBuffer,
    spans: HashMap<u64, Span<T>>,
    traces: HashMap<u128, Trace<T>>,
    string_table: HashMap<u32, T>,
    trace_span_counts: HashMap<u128, usize>,
    tracer_service: T,
    tracer_language: T,
    pid: u32,
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
        pid: u32,
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

                let mut span =
                    maybe_span.ok_or(ChangeBufferError::SpanNotFound(*span_id))?;
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
            .insert(T::from_static_str("process_id"), f64::from(self.pid));

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
            .ok_or(ChangeBufferError::StringNotFound(num))
    }

    fn get_num_arg<U: Copy + FromBytes>(&self, index: &mut usize) -> Result<U> {
        self.change_buffer.read(index)
    }

    fn get_mut_span(&mut self, id: &u64) -> Result<&mut Span<T>> {
        self.spans
            .get_mut(id)
            .ok_or(ChangeBufferError::SpanNotFound(*id))
    }

    pub fn get_span(&self, id: &u64) -> Result<&Span<T>> {
        self.spans
            .get(id)
            .ok_or(ChangeBufferError::SpanNotFound(*id))
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

#[cfg(test)]
mod tests {
    use super::*;

    /// Helper to build the binary buffer layout that flush_change_buffer expects.
    /// Layout: [count: u64][8 bytes padding][operations...]
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
        op_count: u64,
    }

    impl BufBuilder {
        fn new() -> Self {
            // 8 bytes for the count field
            Self {
                data: vec![0u8; 8],
                op_count: 0,
            }
        }

        fn push<T: ToLeBytes>(&mut self, val: T) {
            val.extend_le_bytes(&mut self.data);
        }

        fn push_op_header(&mut self, opcode: OpCode, span_id: u64) {
            self.push(opcode as u64);
            self.push(span_id);
            self.op_count += 1;
        }

        /// Write a Create operation: opcode + span_id + trace_id + parent_id
        fn push_create(&mut self, span_id: u64, trace_id: u128, parent_id: u64) {
            self.push_op_header(OpCode::Create, span_id);
            self.push(trace_id);
            self.push(parent_id);
        }

        fn finalize(&mut self) -> ChangeBuffer {
            self.data[0..8].copy_from_slice(&self.op_count.to_le_bytes());
            unsafe { ChangeBuffer::from_raw_parts(self.data.as_mut_ptr(), self.data.len()) }
        }
    }

    fn make_state(buf: ChangeBuffer) -> ChangeBufferState<&'static str> {
        ChangeBufferState::new(buf, "my-service", "rust", 1234)
    }

    // -- string table --

    #[test]
    fn string_table_insert_and_evict() {
        let mut builder = BufBuilder::new();
        let buf = builder.finalize();
        let mut state = make_state(buf);

        assert_eq!(state.string_table.len(), 0);

        state.string_table_insert_one(1, "hello");
        assert_eq!(state.string_table.len(), 1);

        state.string_table_insert_one(2, "world");
        assert_eq!(state.string_table.len(), 2);
        assert_eq!(state.string_table.get(&1), Some(&"hello"));
        assert_eq!(state.string_table.get(&2), Some(&"world"));

        state.string_table_evict_one(1);
        assert_eq!(state.string_table.len(), 1);
        assert_eq!(state.string_table.get(&1), None);
        assert_eq!(state.string_table.get(&2), Some(&"world"));

        state.string_table_evict_one(2);
        assert_eq!(state.string_table.len(), 0);
    }

    // -- get_span / get_trace --

    #[test]
    fn get_span_missing_returns_error() {
        let mut builder = BufBuilder::new();
        let buf = builder.finalize();
        let state = make_state(buf);
        assert!(state.get_span(&42).is_err());
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
        builder.push_create(100, 200, 50);
        let buf = builder.finalize();
        let mut state = make_state(buf);

        state.flush_change_buffer()?;

        let span = state.get_span(&100)?;
        assert_eq!(span.span_id, 100);
        assert_eq!(span.trace_id, 200);
        assert_eq!(span.parent_id, 50);

        assert!(state.get_trace(&200).is_some());
        assert_eq!(*state.trace_span_counts.get(&200).unwrap(), 1);
        Ok(())
    }

    #[test]
    fn flush_create_multiple_spans_same_trace() -> Result<()> {
        let mut builder = BufBuilder::new();
        builder.push_create(1, 100, 0);
        builder.push_create(2, 100, 1);
        builder.push_create(3, 100, 1);
        let buf = builder.finalize();
        let mut state = make_state(buf);

        state.flush_change_buffer()?;

        assert_eq!(*state.trace_span_counts.get(&100).unwrap(), 3);
        assert!(state.get_span(&1).is_ok());
        assert!(state.get_span(&2).is_ok());
        assert!(state.get_span(&3).is_ok());
        Ok(())
    }

    // -- flush_change_buffer: Set* operations --

    #[test]
    fn flush_set_meta_attr() -> Result<()> {
        let mut builder = BufBuilder::new();
        builder.push_create(1, 100, 0);
        builder.push_op_header(OpCode::SetMetaAttr, 1);
        builder.push(10); // string table key for name
        builder.push(11); // string table key for value
        let buf = builder.finalize();

        let mut state = make_state(buf);
        state.string_table_insert_one(10, "http.method");
        state.string_table_insert_one(11, "GET");

        state.flush_change_buffer()?;

        let span = state.get_span(&1)?;
        assert_eq!(span.meta.get("http.method"), Some(&"GET"));
        Ok(())
    }

    #[test]
    fn flush_set_metric_attr() -> Result<()> {
        let mut builder = BufBuilder::new();
        builder.push_create(1, 100, 0);
        builder.push_op_header(OpCode::SetMetricAttr, 1);
        builder.push(10); // string table key for name
        builder.push(99.5);
        let buf = builder.finalize();

        let mut state = make_state(buf);
        state.string_table_insert_one(10, "my.metric");

        state.flush_change_buffer()?;

        let span = state.get_span(&1)?;
        assert_eq!(span.metrics.get("my.metric"), Some(&99.5));
        Ok(())
    }

    #[test]
    fn flush_set_service_name() -> Result<()> {
        let mut builder = BufBuilder::new();
        builder.push_create(1, 100, 0);
        builder.push_op_header(OpCode::SetServiceName, 1);
        builder.push(10);
        let buf = builder.finalize();

        let mut state = make_state(buf);
        state.string_table_insert_one(10, "web-server");

        state.flush_change_buffer()?;
        assert_eq!(state.get_span(&1)?.service, "web-server");
        Ok(())
    }

    #[test]
    fn flush_set_resource_name() -> Result<()> {
        let mut builder = BufBuilder::new();
        builder.push_create(1, 100, 0);
        builder.push_op_header(OpCode::SetResourceName, 1);
        builder.push(10);
        let buf = builder.finalize();

        let mut state = make_state(buf);
        state.string_table_insert_one(10, "GET /api/users");

        state.flush_change_buffer()?;
        assert_eq!(state.get_span(&1)?.resource, "GET /api/users");
        Ok(())
    }

    #[test]
    fn flush_set_error() -> Result<()> {
        let mut builder = BufBuilder::new();
        builder.push_create(1, 100, 0);
        builder.push_op_header(OpCode::SetError, 1);
        builder.push(1);
        let buf = builder.finalize();

        let mut state = make_state(buf);
        state.flush_change_buffer()?;
        assert_eq!(state.get_span(&1)?.error, 1);
        Ok(())
    }

    #[test]
    fn flush_set_start_and_duration() -> Result<()> {
        let mut builder = BufBuilder::new();
        builder.push_create(1, 100, 0);
        builder.push_op_header(OpCode::SetStart, 1);
        builder.push(1_000_000i64);
        builder.push_op_header(OpCode::SetDuration, 1);
        builder.push(500i64);
        let buf = builder.finalize();

        let mut state = make_state(buf);
        state.flush_change_buffer()?;

        let span = state.get_span(&1)?;
        assert_eq!(span.start, 1_000_000);
        assert_eq!(span.duration, 500);
        Ok(())
    }

    #[test]
    fn flush_set_type_and_name() -> Result<()> {
        let mut builder = BufBuilder::new();
        builder.push_create(1, 100, 0);
        builder.push_op_header(OpCode::SetType, 1);
        builder.push(10);
        builder.push_op_header(OpCode::SetName, 1);
        builder.push(11);
        let buf = builder.finalize();

        let mut state = make_state(buf);
        state.string_table_insert_one(10, "web");
        state.string_table_insert_one(11, "http.request");

        state.flush_change_buffer()?;

        let span = state.get_span(&1)?;
        assert_eq!(span.r#type, "web");
        assert_eq!(span.name, "http.request");
        Ok(())
    }

    // -- flush_change_buffer: trace-level operations --

    #[test]
    fn flush_set_trace_meta_attr() -> Result<()> {
        let mut builder = BufBuilder::new();
        builder.push_create(1, 100, 0);
        builder.push_op_header(OpCode::SetTraceMetaAttr, 1);
        builder.push(10);
        builder.push(11);
        let buf = builder.finalize();

        let mut state = make_state(buf);
        state.string_table_insert_one(10, "env");
        state.string_table_insert_one(11, "production");

        state.flush_change_buffer()?;

        let trace = state.get_trace(&100).unwrap();
        assert_eq!(trace.meta.get("env"), Some(&"production"));
        Ok(())
    }

    #[test]
    fn flush_set_trace_metrics_attr() -> Result<()> {
        let mut builder = BufBuilder::new();
        builder.push_create(1, 100, 0);
        builder.push_op_header(OpCode::SetTraceMetricsAttr, 1);
        builder.push(10);
        builder.push(0.75);
        let buf = builder.finalize();

        let mut state = make_state(buf);
        state.string_table_insert_one(10, "_sampling_priority_v1");

        state.flush_change_buffer()?;

        let trace = state.get_trace(&100).unwrap();
        assert_eq!(trace.metrics.get("_sampling_priority_v1"), Some(&0.75));
        Ok(())
    }

    #[test]
    fn flush_set_trace_origin() -> Result<()> {
        let mut builder = BufBuilder::new();
        builder.push_create(1, 100, 0);
        builder.push_op_header(OpCode::SetTraceOrigin, 1);
        builder.push(10);
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
        builder.push_create(1, 100, 0);
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
        assert!(state.spans.is_empty());
        assert!(state.traces.is_empty());
        Ok(())
    }

    // -- flush_chunk --

    fn create_span_directly(state: &mut ChangeBufferState<&'static str>, span_id: u64, trace_id: u128, parent_id: u64) {
        let span = new_span(span_id, parent_id, trace_id);
        state.spans.insert(span_id, span);
        state.traces.entry(trace_id).or_default();
        *state.trace_span_counts.entry(trace_id).or_insert(0) += 1;
    }

    #[test]
    fn flush_chunk_returns_spans_and_removes_from_state() -> Result<()> {
        let mut builder = BufBuilder::new();
        let buf = builder.finalize();
        let mut state = make_state(buf);

        create_span_directly(&mut state, 1, 100, 0);
        create_span_directly(&mut state, 2, 100, 1);

        let spans = state.flush_chunk(vec![1, 2], false)?;
        assert_eq!(spans.len(), 2);
        assert_eq!(spans[0].span_id, 1);
        assert_eq!(spans[1].span_id, 2);

        // Spans removed from state
        assert!(state.get_span(&1).is_err());
        assert!(state.get_span(&2).is_err());
        Ok(())
    }

    #[test]
    fn flush_chunk_missing_span_returns_error() {
        let mut builder = BufBuilder::new();
        let buf = builder.finalize();
        let mut state = make_state(buf);

        assert!(state.flush_chunk(vec![999], false).is_err());
    }

    #[test]
    fn flush_chunk_local_root_gets_top_level_tag() -> Result<()> {
        let mut builder = BufBuilder::new();
        let buf = builder.finalize();
        let mut state = make_state(buf);

        create_span_directly(&mut state, 1, 100, 0);
        create_span_directly(&mut state, 2, 100, 1);

        let spans = state.flush_chunk(vec![1, 2], true)?;

        // First span (local root) gets _dd.top_level
        assert_eq!(spans[0].metrics.get("_dd.top_level"), Some(&1.0));
        // Second span does not
        assert_eq!(spans[1].metrics.get("_dd.top_level"), None);
        Ok(())
    }

    #[test]
    fn flush_chunk_local_root_gets_sampling_tags() -> Result<()> {
        let mut builder = BufBuilder::new();
        let buf = builder.finalize();
        let mut state = make_state(buf);

        create_span_directly(&mut state, 1, 100, 0);

        // Set sampling decisions on the trace
        let trace = state.traces.get_mut(&100).unwrap();
        trace.sampling_rule_decision = Some(0.5);
        trace.sampling_limit_decision = Some(0.8);
        trace.sampling_agent_decision = Some(1.0);

        let spans = state.flush_chunk(vec![1], true)?;

        assert_eq!(spans[0].metrics.get("_dd.rule_psr"), Some(&0.5));
        assert_eq!(spans[0].metrics.get("_dd.limit_psr"), Some(&0.8));
        assert_eq!(spans[0].metrics.get("_dd.agent_psr"), Some(&1.0));
        Ok(())
    }

    #[test]
    fn flush_chunk_chunk_root_gets_trace_tags() -> Result<()> {
        let mut builder = BufBuilder::new();
        let buf = builder.finalize();
        let mut state = make_state(buf);

        create_span_directly(&mut state, 1, 100, 0);
        create_span_directly(&mut state, 2, 100, 1);

        // Set trace-level meta and metrics
        let trace = state.traces.get_mut(&100).unwrap();
        trace.meta.insert("env", "staging");
        trace.metrics.insert("_sampling_priority_v1", 2.0);

        let spans = state.flush_chunk(vec![1, 2], false)?;

        // First span (chunk root) gets trace tags
        assert_eq!(spans[0].meta.get("env"), Some(&"staging"));
        assert_eq!(
            spans[0].metrics.get("_sampling_priority_v1"),
            Some(&2.0)
        );
        // Second span does not get trace-level tags
        assert_eq!(spans[1].meta.get("env"), None);
        assert_eq!(spans[1].metrics.get("_sampling_priority_v1"), None);
        Ok(())
    }

    // -- process_one_span behaviors (tested via flush_chunk) --

    #[test]
    fn flush_chunk_sets_language_and_pid() -> Result<()> {
        let mut builder = BufBuilder::new();
        let buf = builder.finalize();
        let mut state = make_state(buf);

        create_span_directly(&mut state, 1, 100, 0);

        let spans = state.flush_chunk(vec![1], false)?;
        assert_eq!(spans[0].meta.get("language"), Some(&"rust"));
        assert_eq!(spans[0].metrics.get("process_id"), Some(&1234.0));
        Ok(())
    }

    #[test]
    fn flush_chunk_sets_origin_from_trace() -> Result<()> {
        let mut builder = BufBuilder::new();
        let buf = builder.finalize();
        let mut state = make_state(buf);

        create_span_directly(&mut state, 1, 100, 0);
        state.traces.get_mut(&100).unwrap().origin = Some("synthetics");

        let spans = state.flush_chunk(vec![1], false)?;
        assert_eq!(spans[0].meta.get("_dd.origin"), Some(&"synthetics"));
        Ok(())
    }

    #[test]
    fn flush_chunk_sets_measured_for_non_internal_kind() -> Result<()> {
        let mut builder = BufBuilder::new();
        let buf = builder.finalize();
        let mut state = make_state(buf);

        create_span_directly(&mut state, 1, 100, 0);
        state.spans.get_mut(&1).unwrap().meta.insert("kind", "client");

        let spans = state.flush_chunk(vec![1], false)?;
        assert_eq!(spans[0].metrics.get("_dd.measured"), Some(&1.0));
        Ok(())
    }

    #[test]
    fn flush_chunk_does_not_set_measured_for_internal_kind() -> Result<()> {
        let mut builder = BufBuilder::new();
        let buf = builder.finalize();
        let mut state = make_state(buf);

        create_span_directly(&mut state, 1, 100, 0);
        state
            .spans
            .get_mut(&1)
            .unwrap()
            .meta
            .insert("kind", "internal");

        let spans = state.flush_chunk(vec![1], false)?;
        assert_eq!(spans[0].metrics.get("_dd.measured"), None);
        Ok(())
    }

    #[test]
    fn flush_chunk_sets_base_service_when_service_differs() -> Result<()> {
        let mut builder = BufBuilder::new();
        let buf = builder.finalize();
        let mut state = make_state(buf);

        create_span_directly(&mut state, 1, 100, 0);
        state.spans.get_mut(&1).unwrap().service = "other-service";

        let spans = state.flush_chunk(vec![1], false)?;
        assert_eq!(
            spans[0].meta.get("_dd.base_service"),
            Some(&"my-service")
        );
        Ok(())
    }

    #[test]
    fn flush_chunk_no_base_service_when_service_matches() -> Result<()> {
        let mut builder = BufBuilder::new();
        let buf = builder.finalize();
        let mut state = make_state(buf);

        create_span_directly(&mut state, 1, 100, 0);
        state.spans.get_mut(&1).unwrap().service = "my-service";

        let spans = state.flush_chunk(vec![1], false)?;
        assert_eq!(spans[0].meta.get("_dd.base_service"), None);
        Ok(())
    }

    // -- flush_chunk trace cleanup --

    #[test]
    fn flush_chunk_cleans_up_trace_when_all_spans_flushed() -> Result<()> {
        let mut builder = BufBuilder::new();
        let buf = builder.finalize();
        let mut state = make_state(buf);

        create_span_directly(&mut state, 1, 100, 0);
        create_span_directly(&mut state, 2, 100, 1);

        state.flush_chunk(vec![1, 2], false)?;

        assert!(state.get_trace(&100).is_none());
        assert!(state.trace_span_counts.get(&100).is_none());
        Ok(())
    }

    #[test]
    fn flush_chunk_keeps_trace_when_spans_remain() -> Result<()> {
        let mut builder = BufBuilder::new();
        let buf = builder.finalize();
        let mut state = make_state(buf);

        create_span_directly(&mut state, 1, 100, 0);
        create_span_directly(&mut state, 2, 100, 1);
        create_span_directly(&mut state, 3, 100, 1);

        // Flush only 2 of 3 spans
        state.flush_chunk(vec![1, 2], false)?;

        assert!(state.get_trace(&100).is_some());
        assert_eq!(*state.trace_span_counts.get(&100).unwrap(), 1);
        Ok(())
    }
}

