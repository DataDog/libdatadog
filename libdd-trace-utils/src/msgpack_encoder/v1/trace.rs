// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use crate::span::TraceData;
use rmp::encode::{write_array_len, write_bin, write_bool, write_f64, write_i64, write_map_len, write_sint, write_str, write_u64, write_uint, write_uint8, RmpWrite, ValueWriteError};
use std::borrow::Borrow;
use std::collections::HashMap;
use libdd_trace_protobuf::pb::idx::SpanKind;
use crate::msgpack_decoder::v1::trace::{AnyValueKey, ChunkKey, SpanEventKey, SpanKey, SpanLinkKey, TraceKey};
use crate::span::table::TraceStringRef;
use crate::span::v1::{AttributeAnyValue, Span, SpanEvent, SpanLink, TraceChunk, TraceStaticData, Traces};

pub struct TraceEncoder<'a, W: RmpWrite, T: TraceData> {
    writer: &'a mut W,
    table: &'a TraceStaticData<T>,
    seen_strings: Vec<u32>,
    string_table_index: u32,
}

impl<'a, W: RmpWrite, T: TraceData> TraceEncoder<'a, W, T> {
    /// Creates an encoder for a trace.
    ///
    /// # Arguments
    ///
    /// * `writer` - A RmpWriter compatible with rmp writing functions.
    /// * `table` - The static data.
    ///
    /// # Returns
    ///
    /// * `Ok(())` - Nothing if successful.
    /// * `Err(ValueWriteError)` - An error if the writing fails.
    ///
    /// # Errors
    ///
    /// This function will return any error emitted by the writer.
    pub fn new(writer: &'a mut W, table: &'a TraceStaticData<T>) -> Self {
        Self {
            writer,
            table,
            seen_strings: vec![0; table.strings.len()],
            string_table_index: 0,
        }
    }

    /// Encodes a `Traces` object into a slice of bytes.
    ///
    /// # Arguments
    ///
    /// * `traces` - The traces to encode
    ///
    /// # Returns
    ///
    /// * `Ok(())` - Nothing if successful.
    /// * `Err(ValueWriteError)` - An error if the writing fails.
    ///
    /// # Errors
    ///
    /// This function will return any error emitted by the writer.
    pub fn encode_traces(&mut self, trace: &Traces) -> Result<(), ValueWriteError<W::Error>> {
        let fields = 1 /* chunks */
            + if !trace.container_id.is_empty() { 1 } else { 0 }
            + if !trace.language_name.is_empty() { 1 } else { 0 }
            + if !trace.language_version.is_empty() { 1 } else { 0 }
            + if !trace.tracer_version.is_empty() { 1 } else { 0 }
            + if !trace.runtime_id.is_empty() { 1 } else { 0 }
            + if !trace.env.is_empty() { 1 } else { 0 }
            + if !trace.hostname.is_empty() { 1 } else { 0 }
            + if !trace.app_version.is_empty() { 1 } else { 0 }
            + if !trace.attributes.is_empty() { 1 } else { 0 };
        write_map_len(self.writer, fields)?;

        self.write_keyed_string_ref(TraceKey::ContainerId as u8, trace.container_id)?;
        self.write_keyed_string_ref(TraceKey::LanguageName as u8, trace.language_name)?;
        self.write_keyed_string_ref(TraceKey::LanguageVersion as u8, trace.language_version)?;
        self.write_keyed_string_ref(TraceKey::TracerVersion as u8, trace.tracer_version)?;
        self.write_keyed_string_ref(TraceKey::RuntimeId as u8, trace.runtime_id)?;
        self.write_keyed_string_ref(TraceKey::Env as u8, trace.env)?;
        self.write_keyed_string_ref(TraceKey::Hostname as u8, trace.hostname)?;
        self.write_keyed_string_ref(TraceKey::AppVersion as u8, trace.app_version)?;

        write_uint8(self.writer, TraceKey::Chunks as u8)?;
        self.encode_chunks(trace.chunks.as_ref())?;

        if !trace.attributes.is_empty() {
            write_uint8(self.writer, TraceKey::Attributes as u8)?;
            self.encode_attributes(&trace.attributes)?;
        }

        Ok(())
    }

    fn encode_chunks(&mut self, chunks: &[TraceChunk]) -> Result<(), ValueWriteError<W::Error>> {
        write_array_len(self.writer, chunks.len() as u32)?;

        for chunk in chunks {
            let fields = 2 /* trace_id, spans */
                + if !chunk.origin.is_empty() { 1 } else { 0 }
                + if chunk.dropped_trace { 1 } else { 0 }
                + if chunk.priority != 0 { 1 } else { 0 }
                + if chunk.sampling_mechanism != 0 { 1 } else { 0 }
                + if !chunk.attributes.is_empty() { 1 } else { 0 };
            write_map_len(self.writer, fields)?;

            self.write_keyed_string_ref(ChunkKey::Origin as u8, chunk.origin)?;

            write_uint8(self.writer, ChunkKey::TraceId as u8)?;
            let trace_id = chunk.trace_id.to_be_bytes();
            write_bin(self.writer, trace_id.as_ref())?;

            if chunk.dropped_trace {
                write_uint8(self.writer, ChunkKey::DroppedTrace as u8)?;
                write_bool(self.writer, chunk.dropped_trace).map_err(|e| ValueWriteError::InvalidMarkerWrite(e))?;
            }

            if chunk.priority != 0 {
                write_uint8(self.writer, ChunkKey::Priority as u8)?;
                write_sint(self.writer, chunk.priority as i64)?;
            }

            if chunk.sampling_mechanism != 0 {
                write_uint8(self.writer, ChunkKey::SamplingMechanism as u8)?;
                write_sint(self.writer, chunk.sampling_mechanism as i64)?;
            }

            write_uint8(self.writer, ChunkKey::Spans as u8)?;
            self.encode_spans(&chunk.spans)?;

            if !chunk.attributes.is_empty() {
                write_uint8(self.writer, ChunkKey::Attributes as u8)?;
                self.encode_attributes(&chunk.attributes)?;
            }
        }

        Ok(())
    }

    fn encode_spans(&mut self, spans: &[Span]) -> Result<(), ValueWriteError<W::Error>> {
        write_array_len(self.writer, spans.len() as u32)?;

        for span in spans {
            let fields = 2 /* span_id, start */
                + if !span.service.is_empty() { 1 } else { 0 }
                + if !span.name.is_empty() { 1 } else { 0 }
                + if !span.resource.is_empty() { 1 } else { 0 }
                + if !span.r#type.is_empty() { 1 } else { 0 }
                + if span.parent_id != 0 { 1 } else { 0 }
                + if span.duration != 0 { 1 } else { 0 }
                + if span.error { 1 } else { 0 }
                + if !span.attributes.is_empty() { 1 } else { 0 }
                + if !span.span_links.is_empty() { 1 } else { 0 }
                + if !span.span_events.is_empty() { 1 } else { 0 }
                + if !span.env.is_empty() { 1 } else { 0 }
                + if !span.version.is_empty() { 1 } else { 0 }
                + if !span.component.is_empty() { 1 } else { 0 }
                + if span.kind != SpanKind::Internal { 1 } else { 0 };
            write_map_len(self.writer, fields)?;

            self.write_keyed_string_ref(SpanKey::Service as u8, span.service)?;
            self.write_keyed_string_ref(SpanKey::Name as u8, span.name)?;
            self.write_keyed_string_ref(SpanKey::Resource as u8, span.resource)?;
            self.write_keyed_string_ref(SpanKey::Type as u8, span.r#type)?;
            self.write_keyed_string_ref(SpanKey::Env as u8, span.env)?;
            self.write_keyed_string_ref(SpanKey::Version as u8, span.version)?;
            self.write_keyed_string_ref(SpanKey::Component as u8, span.component)?;

            write_uint8(self.writer, SpanKey::SpanId as u8)?;
            write_u64(self.writer, span.span_id)?;

            write_uint8(self.writer, SpanKey::Start as u8)?;
            write_i64(self.writer, span.start)?;

            if span.parent_id != 0 {
                write_uint8(self.writer, SpanKey::ParentId as u8)?;
                write_u64(self.writer, span.parent_id)?;
            }

            if span.error {
                write_uint8(self.writer, SpanKey::Error as u8)?;
                write_bool(self.writer, span.error).map_err(|e| ValueWriteError::InvalidMarkerWrite(e))?;
            }

            if span.duration != 0 {
                write_uint8(self.writer, SpanKey::Duration as u8)?;
                write_i64(self.writer, span.duration)?;
            }

            if !span.attributes.is_empty() {
                write_uint8(self.writer, SpanKey::Attributes as u8)?;
                self.encode_attributes(&span.attributes)?;
            }

            if !span.span_links.is_empty() {
                write_uint8(self.writer, SpanKey::SpanLinks as u8)?;
                self.encode_span_links(&span.span_links)?;
            }

            if !span.span_events.is_empty() {
                write_uint8(self.writer, SpanKey::SpanEvents as u8)?;
                self.encode_span_events(&span.span_events)?;
            }

            if span.kind != SpanKind::Internal {
                write_uint8(self.writer, SpanKey::Kind as u8)?;
                write_uint8(self.writer, span.kind as u8)?;
            }
        }

        Ok(())
    }

    fn encode_span_links(&mut self, span_links: &[SpanLink]) -> Result<(), ValueWriteError<W::Error>> {
        write_array_len(self.writer, span_links.len() as u32)?;

        for span_link in span_links {
            let fields = 0 /* (span_id and trace_id are optional for span pointer usage) */
                + if span_link.span_id != 0 { 1 } else { 0 }
                + if span_link.trace_id != 0 { 1 } else { 0 }
                + if !span_link.attributes.is_empty() { 1 } else { 0 }
                + if !span_link.tracestate.is_empty() { 1 } else { 0 }
                + if span_link.flags != 0 { 1 } else { 0 };
            write_map_len(self.writer, fields)?;

            self.write_keyed_string_ref(SpanLinkKey::TraceState as u8, span_link.tracestate)?;

            if span_link.span_id != 0 {
                write_uint8(self.writer, SpanLinkKey::SpanId as u8)?;
                write_u64(self.writer, span_link.span_id)?;
            }

            if span_link.trace_id != 0 {
                write_uint8(self.writer, SpanLinkKey::TraceId as u8)?;
                let trace_id = span_link.trace_id.to_be_bytes();
                write_bin(self.writer, trace_id.as_ref())?;
            }

            if span_link.flags != 0 {
                write_uint8(self.writer, SpanLinkKey::Flags as u8)?;
                write_uint(self.writer, span_link.flags as u64)?;
            }

            if !span_link.attributes.is_empty() {
                write_uint8(self.writer, SpanLinkKey::Attributes as u8)?;
                self.encode_attributes(&span_link.attributes)?;
            }
        }

        Ok(())
    }

    fn encode_span_events(&mut self, span_events: &[SpanEvent]) -> Result<(), ValueWriteError<W::Error>> {
        write_array_len(self.writer, span_events.len() as u32)?;

        for span_event in span_events {
            let fields = 2 /* time_unix_nano, name */
                + if !span_event.attributes.is_empty() { 1 } else { 0 };
            write_map_len(self.writer, fields)?;

            write_uint8(self.writer, SpanEventKey::Name as u8)?;
            self.write_string_ref(span_event.name)?;

            write_uint8(self.writer, SpanEventKey::Time as u8)?;
            write_u64(self.writer, span_event.time_unix_nano)?;

            if !span_event.attributes.is_empty() {
                write_uint8(self.writer, SpanEventKey::Attributes as u8)?;
                self.encode_attributes(&span_event.attributes)?;
            }
        }

        Ok(())
    }

    fn encode_attributes(&mut self, attributes: &HashMap<TraceStringRef, AttributeAnyValue>) -> Result<(), ValueWriteError<W::Error>> {
        write_map_len(self.writer, attributes.len() as u32)?;
        for (key, value) in attributes {
            self.write_string_ref(*key)?;
            self.encode_any_value(value)?;
        }
        Ok(())
    }

    fn encode_any_value(&mut self, value: &AttributeAnyValue) -> Result<(), ValueWriteError<W::Error>> {
        match value {
            AttributeAnyValue::String(str) => {
                write_uint8(self.writer, AnyValueKey::String as u8)?;
                self.write_string_ref(*str)
            },
            AttributeAnyValue::Bytes(bytes) => {
                write_uint8(self.writer, AnyValueKey::Bytes as u8)?;
                write_bin(self.writer, bytes.get(&self.table.bytes).borrow())
            },
            AttributeAnyValue::Boolean(bool) => {
                write_uint8(self.writer, AnyValueKey::Bool as u8)?;
                write_bool(self.writer, *bool).map_err(|e| ValueWriteError::InvalidMarkerWrite(e))
            }
            AttributeAnyValue::Integer(int) => {
                write_uint8(self.writer, AnyValueKey::Int64 as u8)?;
                write_i64(self.writer, *int)
            }
            AttributeAnyValue::Double(double) => {
                write_uint8(self.writer, AnyValueKey::Double as u8)?;
                write_f64(self.writer, *double)
            }
            AttributeAnyValue::Array(array) => {
                write_uint8(self.writer, AnyValueKey::Array as u8)?;
                write_array_len(self.writer, array.len() as u32)?;
                for value in array {
                    self.encode_any_value(value)?;
                }
                Ok(())
            }
            AttributeAnyValue::Map(map) => {
                write_uint8(self.writer, AnyValueKey::Map as u8)?;
                self.encode_attributes(map)
            }
        }
    }

    fn write_keyed_string_ref(&mut self, key: u8, strref: TraceStringRef) -> Result<(), ValueWriteError<W::Error>> {
        if strref.is_empty() {
            return Ok(());
        }
        write_uint8(self.writer, key)?;
        self.write_string_ref(strref)
    }

    fn write_string_ref(&mut self, strref: TraceStringRef) -> Result<(), ValueWriteError<W::Error>> {
        if strref.is_empty() {
            write_uint8(self.writer, 0)?;
            return Ok(())
        }

        let string = self.table.strings.get(strref);
        let local_index = strref.get_index();
        let sent_index = self.seen_strings[local_index as usize];
        if sent_index > 0 {
            write_uint(self.writer, sent_index as u64)?;
            return Ok(());
        }
        self.string_table_index += 1;
        self.seen_strings[local_index as usize] = self.string_table_index;
        write_str(self.writer, string.borrow())?;
        Ok(())
    }
}
