use rmp::encode::{write_bin, write_bool, write_f64, write_i32, write_i64, write_str, write_u32, write_u64, write_u8, RmpWrite, ValueWriteError};
use crate::span::{AttributeAnyValue, AttributeArrayValue, Span, SpanText};

/// Encodes a `Span` object into a slice of bytes.
///
/// # Arguments
///
/// * `writer` - A RmpWriter compatible with rmp writing functions.
///
/// # Returns
///
/// * `Ok(()))` - Nothing if successful.
/// * `Err(ValueWriteError)` - An error if the writing fails.
///
/// # Errors
///
/// This function will return any error emitted by the writer.
#[inline(always)]
pub fn encode_span<W: RmpWrite, T: SpanText>(writer: &mut W, span: &Span<T>) -> Result<(), ValueWriteError<W::Error>> {
    let span_len = 7 /* minimal span: trace_id, span_id, service, resource, name, start, duration */
        + (!span.r#type.borrow().is_empty()) as usize
        + (span.parent_id != 0) as usize
        + (span.error != 0) as usize
        + (!span.meta.is_empty()) as usize
        + (!span.metrics.is_empty()) as usize
        + (!span.meta_struct.is_empty()) as usize
        + (!span.span_links.is_empty()) as usize
        + (!span.span_events.is_empty()) as usize;

    rmp::encode::write_map_len(writer, span_len)?;

    write_str(writer,"service")?;
    write_str(writer, span.service.borrow())?;

    write_str(writer, "name")?;
    write_str(writer, span.name.borrow())?;

    write_str(writer, "resource")?;
    write_str(writer, span.resource.borrow())?;

    write_str(writer, "trace_id")?;
    write_u64(writer, span.trace_id)?;

    write_str(writer, "span_id")?;
    write_u64(writer, span.span_id)?;

    if span.parent_id != 0 {
        write_str(writer, "parent_id")?;
        write_u64(writer, span.parent_id)?;
    }

    write_str(writer, "start")?;
    write_i64(writer, span.start)?;

    write_str(writer, "duration")?;
    write_i64(writer, span.duration)?;

    if span.error != 0 {
        write_str(writer, "error")?;
        write_i32(writer, span.error)?;
    }

    if span.meta.len() > 0 {
        write_str(writer, "meta")?;
        rmp::encode::write_map_len(writer, span.meta.len() as u32)?;
        for (k, v) in span.meta.iter() {
            write_str(writer, k.borrow())?;
            write_str(writer, v.borrow())?;
        }
    }

    if span.metrics.len() > 0 {
        write_str(writer, "metrics")?;
        rmp::encode::write_map_len(writer, span.metrics.len() as u32)?;
        for (k, v) in span.metrics.iter() {
            write_str(writer, k.borrow())?;
            write_f64(writer, *v)?;
        }
    }

    if span.r#type.borrow().len() > 0 {
        write_str(writer, "type")?;
        write_str(writer, span.r#type.borrow())?;
    }

    if span.meta_struct.len() > 0 {
        write_str(writer, "meta_struct")?;
        rmp::encode::write_map_len(writer, span.meta_struct.len() as u32)?;
        for (k, v) in span.meta_struct.iter() {
            write_str(writer, k.borrow())?;
            write_bin(writer, v.as_ref())?;
        }
    }

    if span.span_links.len() > 0 {
        write_str(writer, "span_links")?;
        rmp::encode::write_array_len(writer, span.span_links.len() as u32)?;
        for link in span.span_links.iter() {
            let link_len = 3; /* minimal span link: trace_id, trace_id_high, span_id */
                + (link.attributes.len() > 0) as usize
                + (link.tracestate.borrow().len() > 0) as usize
                + (link.flags != 0) as usize;

            rmp::encode::write_map_len(writer, link_len as u32)?;

            write_str(writer, "trace_id")?;
            write_u64(writer, link.trace_id)?;

            write_str(writer, "trace_id_high")?;
            write_u64(writer, link.trace_id_high)?;

            write_str(writer, "span_id")?;
            write_u64(writer, link.span_id)?;

            if link.attributes.len() > 0 {
                write_str(writer, "attributes")?;
                rmp::encode::write_map_len(writer, link.attributes.len() as u32)?;
                for (k, v) in link.attributes.iter() {
                    write_str(writer, k.borrow())?;
                    write_str(writer, (*v).borrow())?;
                }
            }

            if link.tracestate.borrow().len() > 0 {
                write_str(writer, "tracestate")?;
                write_str(writer, link.tracestate.borrow())?;
            }

            if link.flags != 0 {
                write_str(writer, "flags")?;
                write_u32(writer, link.flags)?;
            }
        }
    }

    if span.span_events.len() > 0 {
        write_str(writer, "span_events")?;
        rmp::encode::write_array_len(writer, span.span_events.len() as u32)?;
        for event in span.span_events.iter() {
            let event_len = 2; /* minimal span event: time_unix_nano, name */
                + (event.attributes.len() > 0) as usize;

            rmp::encode::write_map_len(writer, event_len)?;

            write_str(writer, "time_unix_nano")?;
            write_u64(writer, event.time_unix_nano)?;

            write_str(writer, "name")?;
            write_str(writer, event.name.borrow())?;

            if event.attributes.len() > 0 {
                write_str(writer, "attributes")?;
                rmp::encode::write_map_len(writer, event.attributes.len() as u32)?;
                for (k, attribute) in event.attributes.iter() {
                    write_str(writer, k.borrow())?;

                    fn write_array_value<W: RmpWrite, T: SpanText>(writer: &mut W, value: &AttributeArrayValue<T>) -> Result<(), ValueWriteError<W::Error>> {
                        rmp::encode::write_map_len(writer, 2)?;

                        write_str(writer, "type")?;
                        match value {
                            AttributeArrayValue::String(s) => {
                                write_u8(writer, 0)?;
                                write_str(writer, "string_value")?;
                                write_str(writer, s.borrow())?;
                            },
                            AttributeArrayValue::Boolean(bool) => {
                                write_u8(writer, 1)?;
                                write_str(writer, "bool_value")?;
                                write_bool(writer, *bool).map_err(|e| ValueWriteError::InvalidDataWrite(e))?;
                            }
                            AttributeArrayValue::Integer(int) => {
                                write_u8(writer, 2)?;
                                write_str(writer, "int_value")?;
                                write_i64(writer, *int)?;
                            }
                            AttributeArrayValue::Double(double) => {
                                write_u8(writer, 3)?;
                                write_str(writer, "double_value")?;
                                write_f64(writer, *double)?;
                            }
                        };

                        Ok(())
                    }

                    match attribute {
                        AttributeAnyValue::SingleValue(value) => {
                            write_array_value(writer, value)?;
                        }
                        AttributeAnyValue::Array(array) => {
                            rmp::encode::write_map_len(writer, 2)?;

                            write_str(writer, "type")?;
                            write_u8(writer, 4)?;

                            write_str(writer, "array_value")?;
                            rmp::encode::write_map_len(writer, 1)?;

                            write_str(writer, "values")?;
                            rmp::encode::write_array_len(writer, array.len() as u32)?;
                            for v in array.iter() {
                                write_array_value(writer, v)?;
                            }
                        }
                    }
                }
            }
        }
    }

    Ok(())
}
