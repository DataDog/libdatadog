// Copyright 2025-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use crate::msgpack_decoder::decode::error::DecodeError;
use crate::msgpack_decoder::decode::number::read_number_bytes;
use crate::msgpack_decoder::decode::string::{
    handle_null_marker, read_string_bytes, read_string_ref,
};
use crate::span::{AttributeAnyValueBytes, AttributeArrayValueBytes, SpanEventBytes};
use rmp::decode::ValueReadError;
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
pub(crate) fn read_span_events(buf: &mut Bytes) -> Result<Vec<SpanEventBytes>, DecodeError> {
    if let Some(empty_vec) = handle_null_marker(buf, Vec::default) {
        return Ok(empty_vec);
    }

    let len = rmp::decode::read_array_len(unsafe { buf.as_mut_slice() }).map_err(|_| {
        DecodeError::InvalidType("Unable to get array len for span events".to_owned())
    })?;

    let mut vec: Vec<SpanEventBytes> = Vec::with_capacity(len as usize);
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
                format!("Invalid span event key: {}", s).to_owned(),
            )),
        }
    }
}

fn decode_span_event(buf: &mut Bytes) -> Result<SpanEventBytes, DecodeError> {
    let mut event = SpanEventBytes::default();
    let event_size = rmp::decode::read_map_len(unsafe { buf.as_mut_slice() })
        .map_err(|_| DecodeError::InvalidType("Unable to get map len for event size".to_owned()))?;

    for _ in 0..event_size {
        match read_string_ref(unsafe { buf.as_mut_slice() })?.parse::<SpanEventKey>()? {
            SpanEventKey::TimeUnixNano => event.time_unix_nano = read_number_bytes(buf)?,
            SpanEventKey::Name => event.name = read_string_bytes(buf)?,
            SpanEventKey::Attributes => event.attributes = read_attributes_map(buf)?,
        }
    }

    Ok(event)
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

pub fn read_boolean_bytes(buf: &mut Bytes) -> Result<bool, ValueReadError> {
    rmp::decode::read_bool(unsafe { buf.as_mut_slice() })
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
    let mut attribute_type: Option<u8> = None;

    for _ in 0..attribute_size {
        match read_string_ref(unsafe { buf.as_mut_slice() })?.parse::<AttributeAnyKey>()? {
            AttributeAnyKey::Type => attribute_type = Some(read_number_bytes(buf)?),
            AttributeAnyKey::SingleValue(key) => {
                attribute = Some(AttributeAnyValueBytes::SingleValue(get_attribute_from_key(
                    buf, key,
                )?))
            }
            AttributeAnyKey::ArrayValue => {
                attribute = Some(AttributeAnyValueBytes::Array(read_attributes_array(buf)?))
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

fn read_attributes_array(buf: &mut Bytes) -> Result<Vec<AttributeArrayValueBytes>, DecodeError> {
    if let Some(empty_vec) = handle_null_marker(buf, Vec::default) {
        return Ok(empty_vec);
    }

    let len = rmp::decode::read_array_len(unsafe { buf.as_mut_slice() }).map_err(|_| {
        DecodeError::InvalidType("Unable to get array len for event attributes".to_owned())
    })?;

    let mut vec: Vec<AttributeArrayValueBytes> = Vec::with_capacity(len as usize);
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
                format!("Invalid attribute key: {}", s).to_owned(),
            )),
        }
    }
}

fn get_attribute_from_key(
    buf: &mut Bytes,
    key: AttributeArrayKey,
) -> Result<AttributeArrayValueBytes, DecodeError> {
    match key {
        AttributeArrayKey::StringValue => {
            Ok(AttributeArrayValueBytes::String(read_string_bytes(buf)?))
        }
        AttributeArrayKey::BoolValue => {
            let boolean = read_boolean_bytes(buf);
            if let Ok(value) = boolean {
                match value {
                    true => Ok(AttributeArrayValueBytes::Boolean(true)),
                    false => Ok(AttributeArrayValueBytes::Boolean(false)),
                }
            } else {
                Err(DecodeError::InvalidType("Invalid boolean field".to_owned()))
            }
        }
        AttributeArrayKey::IntValue => {
            Ok(AttributeArrayValueBytes::Integer(read_number_bytes(buf)?))
        }
        AttributeArrayKey::DoubleValue => {
            Ok(AttributeArrayValueBytes::Double(read_number_bytes(buf)?))
        }
        _ => Err(DecodeError::InvalidFormat("Invalid attribute".to_owned())),
    }
}

fn decode_attribute_array(
    buf: &mut Bytes,
    array_type: Option<u8>,
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
    let mut attribute_type: Option<u8> = None;

    for _ in 0..attribute_size {
        match read_string_ref(unsafe { buf.as_mut_slice() })?.parse::<AttributeArrayKey>()? {
            AttributeArrayKey::Type => attribute_type = Some(read_number_bytes(buf)?),
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
