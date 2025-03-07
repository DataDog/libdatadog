// Copyright 2024-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use crate::msgpack_decoder::decode::error::DecodeError;
use crate::msgpack_decoder::decode::number::read_number_bytes;
use crate::msgpack_decoder::decode::string::{
    handle_null_marker, read_string_bytes, read_string_ref,
};
use crate::span::v04::{AttributeAnyValueBytes, AttributeArrayValueBytes, SpanEventBytes};
use rmp::Marker;
use std::collections::HashMap;
use std::str::FromStr;
use tinybytes::{Bytes, BytesString};

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
/// ```
pub(crate) fn read_span_events(buf: &mut Bytes) -> Result<Vec<SpanEventBytes>, DecodeError> {
    if let Some(empty_vec) = handle_null_marker(buf, Vec::default) {
        return Ok(empty_vec);
    }

    match rmp::decode::read_marker(unsafe { buf.as_mut_slice() }).map_err(|_| {
        DecodeError::InvalidFormat("Unable to read marker for span events".to_owned())
    })? {
        Marker::FixArray(len) => {
            let mut vec: Vec<SpanEventBytes> = Vec::with_capacity(len.into());
            for _ in 0..len {
                vec.push(decode_span_event(buf)?);
            }
            Ok(vec)
        }
        _ => Err(DecodeError::InvalidType(
            "Unable to read span event from buffer".to_owned(),
        )),
    }
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
                format!("Invalid span event key: {}", s).to_owned(),
            )),
        }
    }
}

fn decode_span_event(buf: &mut Bytes) -> Result<SpanEventBytes, DecodeError> {
    let mut span = SpanEventBytes::default();
    let span_size = rmp::decode::read_map_len(unsafe { buf.as_mut_slice() })
        .map_err(|_| DecodeError::InvalidType("Unable to get map len for span size".to_owned()))?;

    for _ in 0..span_size {
        match read_string_ref(unsafe { buf.as_mut_slice() })?.parse::<SpanEventKey>()? {
            SpanEventKey::TimeUnixNano => span.time_unix_nano = read_number_bytes(buf)?,
            SpanEventKey::Name => span.name = read_string_bytes(buf)?,
            SpanEventKey::Attributes => span.attributes = read_attributes_map(buf)?,
        }
    }

    Ok(span)
}

fn read_attributes_map(
    buf: &mut Bytes,
) -> Result<HashMap<BytesString, AttributeAnyValueBytes>, DecodeError> {
    let len = rmp::decode::read_map_len(unsafe { buf.as_mut_slice() })
        .map_err(|_| DecodeError::InvalidType("Unable to get map len for attributes".to_owned()))?;

    let mut map = HashMap::with_capacity(len.try_into().expect("Unable to cast map len to usize"));
    for _ in 0..len {
        let key = read_string_bytes(buf)?;
        let value = decode_attribute_any(buf)?;
        map.insert(key, value);
    }

    Ok(map)
}

#[derive(Debug, PartialEq)]
enum AttributeAnyKey {
    Type,
    StringValue,
    BoolValue,
    IntValue,
    DoubleValue,
    ArrayValue,
}

impl FromStr for AttributeAnyKey {
    type Err = DecodeError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "type" => Ok(AttributeAnyKey::Type),
            "string_value" => Ok(AttributeAnyKey::StringValue),
            "bool_value" => Ok(AttributeAnyKey::BoolValue),
            "int_value" => Ok(AttributeAnyKey::IntValue),
            "double_value" => Ok(AttributeAnyKey::DoubleValue),
            "array_value" => Ok(AttributeAnyKey::ArrayValue),
            _ => Err(DecodeError::InvalidFormat(
                format!("Invalid attribute any key: {}", s).to_owned(),
            )),
        }
    }
}

fn decode_attribute_any(buf: &mut Bytes) -> Result<AttributeAnyValueBytes, DecodeError> {
    let mut attribute: Option<AttributeAnyValueBytes> = None;
    let attribute_size =
        rmp::decode::read_map_len(unsafe { buf.as_mut_slice() }).map_err(|_| {
            DecodeError::InvalidType("Unable to get map len for attribute size".to_owned())
        })?;

    if attribute_size != 2 {
        return Err(DecodeError::InvalidFormat(
            "Invalid number of field for an attribute".to_owned(),
        ));
    }
    let mut attribute_type = 5;

    for _ in 0..attribute_size {
        match read_string_ref(unsafe { buf.as_mut_slice() })?.parse::<AttributeAnyKey>()? {
            AttributeAnyKey::Type => attribute_type = read_number_bytes(buf)?,
            AttributeAnyKey::StringValue => {
                attribute = Some(AttributeAnyValueBytes::String(read_string_bytes(buf)?))
            }
            AttributeAnyKey::BoolValue => {
                let boolean = read_string_bytes(buf)?;
                match boolean.as_str() {
                    "true" => attribute = Some(AttributeAnyValueBytes::Boolean(true)),
                    "false" => attribute = Some(AttributeAnyValueBytes::Boolean(false)),
                    _ => return Err(DecodeError::InvalidType("Invalid boolean field".to_owned())),
                }
            }
            AttributeAnyKey::IntValue => {
                attribute = Some(AttributeAnyValueBytes::Integer(read_number_bytes(buf)?))
            }
            AttributeAnyKey::DoubleValue => {
                attribute = Some(AttributeAnyValueBytes::Double(read_number_bytes(buf)?))
            }
            AttributeAnyKey::ArrayValue => {
                attribute = Some(AttributeAnyValueBytes::Array(read_attributes_array(buf)?))
            }
        }
    }

    if let Some(value) = attribute {
        if type_from_attribute(&value) == attribute_type {
            Ok(value)
        } else {
            Err(DecodeError::InvalidType(
                "Type mismatch for attribute".to_owned(),
            ))
        }
    } else {
        Err(DecodeError::InvalidFormat("Invalid attribute".to_owned()))
    }
}

fn type_from_attribute(attribute: &AttributeAnyValueBytes) -> u8 {
    match attribute {
        AttributeAnyValueBytes::String(_) => 0,
        AttributeAnyValueBytes::Boolean(_) => 1,
        AttributeAnyValueBytes::Integer(_) => 2,
        AttributeAnyValueBytes::Double(_) => 3,
        AttributeAnyValueBytes::Array(_) => 4,
    }
}

fn read_attributes_array(buf: &mut Bytes) -> Result<Vec<AttributeArrayValueBytes>, DecodeError> {
    if let Some(empty_vec) = handle_null_marker(buf, Vec::default) {
        return Ok(empty_vec);
    }

    match rmp::decode::read_marker(unsafe { buf.as_mut_slice() }).map_err(|_| {
        DecodeError::InvalidFormat("Unable to read marker for attributes".to_owned())
    })? {
        Marker::FixArray(len) => {
            let mut vec: Vec<AttributeArrayValueBytes> = Vec::with_capacity(len.into());
            let first = decode_attribute_array(buf, 4)?;
            let array_type = type_from_attribute_array(&first);
            vec.push(first);
            for _ in 1..len {
                vec.push(decode_attribute_array(buf, array_type)?);
            }
            Ok(vec)
        }
        _ => Err(DecodeError::InvalidType(
            "Unable to read attribute from buffer".to_owned(),
        )),
    }
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
                format!("Invalid attribute array key: {}", s).to_owned(),
            )),
        }
    }
}

fn decode_attribute_array(
    buf: &mut Bytes,
    array_type: u8,
) -> Result<AttributeArrayValueBytes, DecodeError> {
    let mut attribute: Option<AttributeArrayValueBytes> = None;
    let attribute_size =
        rmp::decode::read_map_len(unsafe { buf.as_mut_slice() }).map_err(|_| {
            DecodeError::InvalidType("Unable to get map len for attribute size".to_owned())
        })?;

    if attribute_size != 2 {
        return Err(DecodeError::InvalidFormat(
            "Invalid number of field for an attribute".to_owned(),
        ));
    }
    let mut attribute_type = 5;

    for _ in 0..attribute_size {
        match read_string_ref(unsafe { buf.as_mut_slice() })?.parse::<AttributeArrayKey>()? {
            AttributeArrayKey::Type => attribute_type = read_number_bytes(buf)?,
            AttributeArrayKey::StringValue => {
                attribute = Some(AttributeArrayValueBytes::String(read_string_bytes(buf)?))
            }
            AttributeArrayKey::BoolValue => {
                let boolean = read_string_bytes(buf)?;
                match boolean.as_str() {
                    "true" => attribute = Some(AttributeArrayValueBytes::Boolean(true)),
                    "false" => attribute = Some(AttributeArrayValueBytes::Boolean(false)),
                    _ => return Err(DecodeError::InvalidType("Invalid boolean field".to_owned())),
                }
            }
            AttributeArrayKey::IntValue => {
                attribute = Some(AttributeArrayValueBytes::Integer(read_number_bytes(buf)?))
            }
            AttributeArrayKey::DoubleValue => {
                attribute = Some(AttributeArrayValueBytes::Double(read_number_bytes(buf)?))
            }
        }
    }

    if let Some(value) = attribute {
        if type_from_attribute_array(&value) == attribute_type {
            if array_type == 4 || array_type == attribute_type {
                Ok(value)
            } else {
                Err(DecodeError::InvalidType(
                    "Array must have same type element".to_owned(),
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

fn type_from_attribute_array(attribute: &AttributeArrayValueBytes) -> u8 {
    match attribute {
        AttributeArrayValueBytes::String(_) => 0,
        AttributeArrayValueBytes::Boolean(_) => 1,
        AttributeArrayValueBytes::Integer(_) => 2,
        AttributeArrayValueBytes::Double(_) => 3,
    }
}
