// Copyright 2024-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use super::error::DecodeError;
use rmp::{decode::RmpRead, Marker};

pub enum Number {
    U8(u8),
    U32(u32),
    U64(u64),
    I8(i8),
    I32(i32),
    I64(i64),
    F64(f64),
}

impl TryFrom<Number> for u8 {
    type Error = DecodeError;
    fn try_from(value: Number) -> Result<Self, Self::Error> {
        match value {
            Number::U8(val) => Ok(val),
            Number::I8(val) => Ok(val as u8),
            _ => Err(DecodeError::WrongConversion),
        }
    }
}

impl TryFrom<Number> for i8 {
    type Error = DecodeError;
    fn try_from(value: Number) -> Result<Self, Self::Error> {
        match value {
            Number::U8(val) => Ok(val as i8),
            Number::I8(val) => Ok(val),
            _ => Err(DecodeError::WrongConversion),
        }
    }
}

impl TryFrom<Number> for u32 {
    type Error = DecodeError;
    fn try_from(value: Number) -> Result<Self, Self::Error> {
        match value {
            Number::U8(val) => Ok(val as u32),
            Number::U32(val) => Ok(val),
            _ => Err(DecodeError::WrongConversion),
        }
    }
}

impl TryFrom<Number> for u64 {
    type Error = DecodeError;
    fn try_from(value: Number) -> Result<Self, Self::Error> {
        match value {
            Number::U8(val) => Ok(val as u64),
            Number::U32(val) => Ok(val as u64),
            Number::U64(val) => Ok(val),
            _ => Err(DecodeError::WrongConversion),
        }
    }
}

impl TryFrom<Number> for i64 {
    type Error = DecodeError;
    fn try_from(value: Number) -> Result<Self, Self::Error> {
        match value {
            Number::U8(val) => Ok(val as i64),
            Number::U32(val) => Ok(val as i64),
            Number::I8(val) => Ok(val as i64),
            Number::I32(val) => Ok(val as i64),
            Number::I64(val) => Ok(val),
            _ => Err(DecodeError::WrongConversion),
        }
    }
}

impl TryFrom<Number> for i32 {
    type Error = DecodeError;
    fn try_from(value: Number) -> Result<Self, Self::Error> {
        match value {
            Number::U8(val) => Ok(val as i32),
            Number::I8(val) => Ok(val as i32),
            Number::U32(val) => Ok(val as i32),
            Number::I32(val) => Ok(val),
            _ => Err(DecodeError::WrongConversion),
        }
    }
}

impl TryFrom<Number> for f64 {
    type Error = DecodeError;
    fn try_from(value: Number) -> Result<Self, Self::Error> {
        match value {
            Number::U8(val) => Ok(val as f64),
            Number::U32(val) => Ok(val as f64),
            Number::U64(val) => Ok(val as f64),
            Number::I8(val) => Ok(val as f64),
            Number::I32(val) => Ok(val as f64),
            Number::I64(val) => Ok(val as f64),
            Number::F64(val) => Ok(val),
        }
    }
}

pub fn read_number(buf: &mut &[u8]) -> Result<Number, DecodeError> {
    match rmp::decode::read_marker(buf).map_err(|_| DecodeError::WrongFormat)? {
        Marker::FixPos(val) => Ok(Number::U8(val)),
        Marker::FixNeg(val) => Ok(Number::I8(val)),
        Marker::U8 => Ok(Number::U8(
            buf.read_data_u8().map_err(|_| DecodeError::IOError)?,
        )),
        Marker::U16 => Ok(Number::U32(
            buf.read_data_u16().map_err(|_| DecodeError::IOError)? as u32,
        )),
        Marker::U32 => Ok(Number::U32(
            buf.read_data_u32().map_err(|_| DecodeError::IOError)?,
        )),
        Marker::U64 => Ok(Number::U64(
            buf.read_data_u64().map_err(|_| DecodeError::IOError)?,
        )),
        Marker::I8 => Ok(Number::I32(
            buf.read_data_i8().map_err(|_| DecodeError::IOError)? as i32,
        )),
        Marker::I16 => Ok(Number::I32(
            buf.read_data_i16().map_err(|_| DecodeError::IOError)? as i32,
        )),
        Marker::I32 => Ok(Number::I32(
            buf.read_data_i32().map_err(|_| DecodeError::IOError)?,
        )),
        Marker::I64 => Ok(Number::I64(
            buf.read_data_i64().map_err(|_| DecodeError::IOError)?,
        )),
        Marker::F32 => Ok(Number::F64(
            buf.read_data_f32().map_err(|_| DecodeError::IOError)? as f64,
        )),
        Marker::F64 => Ok(Number::F64(
            buf.read_data_f64().map_err(|_| DecodeError::IOError)?,
        )),
        _ => Err(DecodeError::WrongType),
    }
}

#[cfg(test)]
mod tests {
    use std::f64;

    use super::*;

    #[test]
    fn read_number_success() {
        let values = (1, -1_i8, i32::MIN, u32::MAX, i64::MIN, u64::MAX, f64::MAX);

        assert_eq!(
            values.0,
            TryInto::<u8>::try_into(
                read_number(&mut rmp_serde::to_vec_named(&values.0).unwrap().as_ref()).unwrap()
            )
            .unwrap()
        );
        assert_eq!(
            values.1,
            TryInto::<i8>::try_into(
                read_number(&mut rmp_serde::to_vec_named(&values.1).unwrap().as_ref()).unwrap()
            )
            .unwrap()
        );
        assert_eq!(
            values.2,
            TryInto::<i32>::try_into(
                read_number(&mut rmp_serde::to_vec_named(&values.2).unwrap().as_ref()).unwrap()
            )
            .unwrap()
        );
        assert_eq!(
            values.3,
            TryInto::<u32>::try_into(
                read_number(&mut rmp_serde::to_vec_named(&values.3).unwrap().as_ref()).unwrap()
            )
            .unwrap()
        );
        assert_eq!(
            values.4,
            TryInto::<i64>::try_into(
                read_number(&mut rmp_serde::to_vec_named(&values.4).unwrap().as_ref()).unwrap()
            )
            .unwrap()
        );
        assert_eq!(
            values.5,
            TryInto::<u64>::try_into(
                read_number(&mut rmp_serde::to_vec_named(&values.5).unwrap().as_ref()).unwrap()
            )
            .unwrap()
        );
        assert_eq!(
            values.6,
            TryInto::<f64>::try_into(
                read_number(&mut rmp_serde::to_vec_named(&values.6).unwrap().as_ref()).unwrap()
            )
            .unwrap()
        );
    }
}
