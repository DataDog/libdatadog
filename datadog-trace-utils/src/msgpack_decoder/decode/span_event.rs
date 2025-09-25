// Copyright 2025-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use crate::msgpack_decoder::decode::buffer::Buffer;
use crate::msgpack_decoder::decode::error::DecodeError;
use crate::msgpack_decoder::decode::number::read_number;
use crate::msgpack_decoder::decode::string::handle_null_marker;
use crate::span::v04::{AttributeAnyValue, AttributeArrayValue, SpanEvent};
use crate::span::TraceData;
use std::borrow::Borrow;
use std::collections::HashMap;
use std::str::FromStr;

/// Reads a slice of bytes and decodes it into a vector of `SpanEvent` objects.
///
/// # Arguments
///
/// * `buf` - A mutable reference to a slice of bytes containing the encoded data.
///
/// # Returns
///
/// * `Ok(Vec<SpanEvent>)` - A vector of decoded `SpanEvent` objects if successful.
/// * `Err(DecodeError)` - An error if the decoding process fails.
///
/// # Errors
///
/// This function will return an error if:
/// - The marker for the array length cannot be read.
/// - Any `SpanEvent` cannot be decoded.
pub(crate) fn read_span_events<T: TraceData>(
    buf: &mut Buffer<T>,
) -> Result<Vec<SpanEvent<T>>, DecodeError> {
    if handle_null_marker(buf) {
        return Ok(Vec::default());
    }

    let len = rmp::decode::read_array_len(buf.as_mut_slice()).map_err(|_| {
        DecodeError::InvalidType("Unable to get array len for span events".to_owned())
    })?;

    let mut vec: Vec<SpanEvent<T>> = Vec::with_capacity(len as usize);
    for _ in 0..len {
        vec.push(decode_span_event(buf)?);
    }
    Ok(vec)
}
#[derive(Debug, PartialEq)]
enum SpanEventKey {
    TimeUnixNano,
    Name,
    Attributes,
}

impl FromStr for SpanEventKey {
    type Err = DecodeError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "time_unix_nano" => Ok(SpanEventKey::TimeUnixNano),
            "name" => Ok(SpanEventKey::Name),
            "attributes" => Ok(SpanEventKey::Attributes),
            _ => Err(DecodeError::InvalidFormat(
                format!("Invalid span event key: {s}").to_owned(),
            )),
        }
    }
}

fn decode_span_event<T: TraceData>(buf: &mut Buffer<T>) -> Result<SpanEvent<T>, DecodeError> {
    let mut event = SpanEvent::default();
    let event_size = rmp::decode::read_map_len(buf.as_mut_slice())
        .map_err(|_| DecodeError::InvalidType("Unable to get map len for event size".to_owned()))?;

    for _ in 0..event_size {
        match buf.read_string()?.borrow().parse::<SpanEventKey>()? {
            SpanEventKey::TimeUnixNano => event.time_unix_nano = read_number(buf)?,
            SpanEventKey::Name => event.name = buf.read_string()?,
            SpanEventKey::Attributes => event.attributes = read_attributes_map(buf)?,
        }
    }

    Ok(event)
}

fn read_attributes_map<T: TraceData>(
    buf: &mut Buffer<T>,
) -> Result<HashMap<T::Text, AttributeAnyValue<T>>, DecodeError> {
    let len = rmp::decode::read_map_len(buf.as_mut_slice())
        .map_err(|_| DecodeError::InvalidType("Unable to get map len for attributes".to_owned()))?;

    #[allow(clippy::expect_used)]
    let mut map = HashMap::with_capacity(len.try_into().expect("Unable to cast map len to usize"));
    for _ in 0..len {
        let key = buf.read_string()?;
        let value = decode_attribute_any(buf)?;
        map.insert(key, value);
    }

    Ok(map)
}

#[derive(Debug, PartialEq)]
enum AttributeAnyKey {
    Type,
    SingleValue(AttributeArrayKey),
    ArrayValue,
}

impl FromStr for AttributeAnyKey {
    type Err = DecodeError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "type" => Ok(AttributeAnyKey::Type),
            "array_value" => Ok(AttributeAnyKey::ArrayValue),
            s => {
                let r = AttributeArrayKey::from_str(s);
                match r {
                    Ok(key) => Ok(AttributeAnyKey::SingleValue(key)),
                    Err(e) => Err(e),
                }
            }
        }
    }
}

fn decode_attribute_any<T: TraceData>(
    buf: &mut Buffer<T>,
) -> Result<AttributeAnyValue<T>, DecodeError> {
    let mut attribute: Option<AttributeAnyValue<T>> = None;
    let attribute_size = rmp::decode::read_map_len(buf.as_mut_slice()).map_err(|_| {
        DecodeError::InvalidType("Unable to get map len for attribute size".to_owned())
    })?;

    if attribute_size != 2 {
        return Err(DecodeError::InvalidFormat(
            "Invalid number of field for an attribute".to_owned(),
        ));
    }
    let mut attribute_type: Option<u8> = None;

    for _ in 0..attribute_size {
        match buf.read_string()?.borrow().parse::<AttributeAnyKey>()? {
            AttributeAnyKey::Type => attribute_type = Some(read_number(buf)?),
            AttributeAnyKey::SingleValue(key) => {
                attribute = Some(AttributeAnyValue::SingleValue(get_attribute_from_key(
                    buf, key,
                )?))
            }
            AttributeAnyKey::ArrayValue => {
                attribute = Some(AttributeAnyValue::Array(read_attributes_array(buf)?))
            }
        }
    }

    if let Some(value) = attribute {
        if let Some(attribute_type) = attribute_type {
            let value_type: u8 = (&value).into();
            if attribute_type == value_type {
                Ok(value)
            } else {
                Err(DecodeError::InvalidFormat(
                    "No type for attribute".to_owned(),
                ))
            }
        } else {
            Err(DecodeError::InvalidType(
                "Type mismatch for attribute".to_owned(),
            ))
        }
    } else {
        Err(DecodeError::InvalidFormat("Invalid attribute".to_owned()))
    }
}

fn read_attributes_array<T: TraceData>(
    buf: &mut Buffer<T>,
) -> Result<Vec<AttributeArrayValue<T>>, DecodeError> {
    if handle_null_marker(buf) {
        return Ok(Vec::default());
    }

    let map_len = rmp::decode::read_map_len(buf.as_mut_slice()).map_err(|_| {
        DecodeError::InvalidType(
            "Unable to get map len for event attributes array_value object".to_owned(),
        )
    })?;

    if map_len != 1 {
        return Err(DecodeError::InvalidFormat(
            "event attributes array_value object should only have 'values' field".to_owned(),
        ));
    }

    let key = buf.read_string()?;
    if key.borrow() != "values" {
        return Err(DecodeError::InvalidFormat(
            "Expected 'values' field in event attributes array_value object".to_owned(),
        ));
    }

    let len = rmp::decode::read_array_len(buf.as_mut_slice()).map_err(|_| {
        DecodeError::InvalidType(
            "Unable to get array len for event attributes values field".to_owned(),
        )
    })?;

    let mut vec: Vec<AttributeArrayValue<T>> = Vec::with_capacity(len as usize);
    if len > 0 {
        let first = decode_attribute_array(buf, None)?;
        let array_type = (&first).into();
        vec.push(first);
        for _ in 1..len {
            vec.push(decode_attribute_array(buf, Some(array_type))?);
        }
    }
    Ok(vec)
}

#[derive(Debug, PartialEq)]
enum AttributeArrayKey {
    Type,
    StringValue,
    BoolValue,
    IntValue,
    DoubleValue,
}

impl FromStr for AttributeArrayKey {
    type Err = DecodeError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "type" => Ok(AttributeArrayKey::Type),
            "string_value" => Ok(AttributeArrayKey::StringValue),
            "bool_value" => Ok(AttributeArrayKey::BoolValue),
            "int_value" => Ok(AttributeArrayKey::IntValue),
            "double_value" => Ok(AttributeArrayKey::DoubleValue),
            _ => Err(DecodeError::InvalidFormat(
                format!("Invalid attribute key: {s}").to_owned(),
            )),
        }
    }
}

fn get_attribute_from_key<T: TraceData>(
    buf: &mut Buffer<T>,
    key: AttributeArrayKey,
) -> Result<AttributeArrayValue<T>, DecodeError> {
    match key {
        AttributeArrayKey::StringValue => Ok(AttributeArrayValue::String(buf.read_string()?)),
        AttributeArrayKey::BoolValue => {
            let boolean = rmp::decode::read_bool(buf.as_mut_slice());
            if let Ok(value) = boolean {
                match value {
                    true => Ok(AttributeArrayValue::Boolean(true)),
                    false => Ok(AttributeArrayValue::Boolean(false)),
                }
            } else {
                Err(DecodeError::InvalidType("Invalid boolean field".to_owned()))
            }
        }
        AttributeArrayKey::IntValue => Ok(AttributeArrayValue::Integer(read_number(buf)?)),
        AttributeArrayKey::DoubleValue => Ok(AttributeArrayValue::Double(read_number(buf)?)),
        _ => Err(DecodeError::InvalidFormat("Invalid attribute".to_owned())),
    }
}

fn decode_attribute_array<T: TraceData>(
    buf: &mut Buffer<T>,
    array_type: Option<u8>,
) -> Result<AttributeArrayValue<T>, DecodeError> {
    let mut attribute: Option<AttributeArrayValue<T>> = None;
    let attribute_size = rmp::decode::read_map_len(buf.as_mut_slice()).map_err(|_| {
        DecodeError::InvalidType("Unable to get map len for attribute size".to_owned())
    })?;

    if attribute_size != 2 {
        return Err(DecodeError::InvalidFormat(
            "Invalid number of field for an attribute".to_owned(),
        ));
    }
    let mut attribute_type: Option<u8> = None;

    for _ in 0..attribute_size {
        match buf.read_string()?.borrow().parse::<AttributeArrayKey>()? {
            AttributeArrayKey::Type => attribute_type = Some(read_number(buf)?),
            key => attribute = Some(get_attribute_from_key(buf, key)?),
        }
    }

    if let Some(value) = attribute {
        if let Some(attribute_type) = attribute_type {
            let value_type: u8 = (&value).into();
            if attribute_type == value_type {
                if let Some(array_type) = array_type {
                    if array_type != attribute_type {
                        return Err(DecodeError::InvalidType(
                            "Array must have same type element".to_owned(),
                        ));
                    }
                }
                Ok(value)
            } else {
                Err(DecodeError::InvalidFormat(
                    "No type for attribute".to_owned(),
                ))
            }
        } else {
            Err(DecodeError::InvalidType(
                "Type mismatch for attribute".to_owned(),
            ))
        }
    } else {
        Err(DecodeError::InvalidFormat("Invalid attribute".to_owned()))
    }
}

#[cfg(test)]
mod tests {
    use super::AttributeAnyKey;
    use super::AttributeArrayKey;
    use super::SpanEventKey;
    use crate::msgpack_decoder::decode::error::DecodeError;
    use std::str::FromStr;

    #[test]
    fn test_span_event_key_from_str() {
        // Valid cases
        assert_eq!(
            SpanEventKey::from_str("time_unix_nano").unwrap(),
            SpanEventKey::TimeUnixNano
        );
        assert_eq!(SpanEventKey::from_str("name").unwrap(), SpanEventKey::Name);
        assert_eq!(
            SpanEventKey::from_str("attributes").unwrap(),
            SpanEventKey::Attributes
        );

        // Invalid case
        assert!(matches!(
            SpanEventKey::from_str("invalid_key"),
            Err(DecodeError::InvalidFormat(_))
        ));
    }

    #[test]
    fn test_attribute_any_key_from_str() {
        // Valid cases
        assert_eq!(
            AttributeAnyKey::from_str("type").unwrap(),
            AttributeAnyKey::Type
        );
        assert_eq!(
            AttributeAnyKey::from_str("string_value").unwrap(),
            AttributeAnyKey::SingleValue(AttributeArrayKey::StringValue)
        );
        assert_eq!(
            AttributeAnyKey::from_str("bool_value").unwrap(),
            AttributeAnyKey::SingleValue(AttributeArrayKey::BoolValue)
        );
        assert_eq!(
            AttributeAnyKey::from_str("int_value").unwrap(),
            AttributeAnyKey::SingleValue(AttributeArrayKey::IntValue)
        );
        assert_eq!(
            AttributeAnyKey::from_str("double_value").unwrap(),
            AttributeAnyKey::SingleValue(AttributeArrayKey::DoubleValue)
        );
        assert_eq!(
            AttributeAnyKey::from_str("array_value").unwrap(),
            AttributeAnyKey::ArrayValue
        );

        // Invalid case
        assert!(matches!(
            AttributeAnyKey::from_str("invalid_key"),
            Err(DecodeError::InvalidFormat(_))
        ));
    }

    #[test]
    fn test_attribute_array_key_from_str() {
        // Valid cases
        assert_eq!(
            AttributeArrayKey::from_str("type").unwrap(),
            AttributeArrayKey::Type
        );
        assert_eq!(
            AttributeArrayKey::from_str("string_value").unwrap(),
            AttributeArrayKey::StringValue
        );
        assert_eq!(
            AttributeArrayKey::from_str("bool_value").unwrap(),
            AttributeArrayKey::BoolValue
        );
        assert_eq!(
            AttributeArrayKey::from_str("int_value").unwrap(),
            AttributeArrayKey::IntValue
        );
        assert_eq!(
            AttributeArrayKey::from_str("double_value").unwrap(),
            AttributeArrayKey::DoubleValue
        );

        // Invalid case
        assert!(matches!(
            AttributeArrayKey::from_str("invalid_key"),
            Err(DecodeError::InvalidFormat(_))
        ));
    }
}
