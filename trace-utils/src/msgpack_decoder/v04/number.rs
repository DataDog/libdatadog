// Copyright 2024-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use super::error::DecodeError;
use rmp::{decode::RmpRead, Marker};
use std::fmt;
use tinybytes::Bytes;

#[derive(Debug, PartialEq)]
pub enum Number {
    Unsigned(u64),
    Signed(i64),
    Float(f64),
}

impl fmt::Display for Number {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Number::Signed(val) => write!(f, "{}", val),
            Number::Unsigned(val) => write!(f, "{}", val),
            Number::Float(val) => write!(f, "{}", val),
        }
    }
}

impl Number {
    pub fn bounded_int_conversion<T>(
        self,
        lower_bound: T,
        upper_bound: Option<T>,
    ) -> Result<T, DecodeError>
    where
        T: TryInto<i64> + TryInto<u64> + TryInto<i32> + Copy + fmt::Display,
        i64: TryInto<T>,
        u64: TryInto<T>,
        <T as TryInto<i64>>::Error: fmt::Debug,
        <T as TryInto<u64>>::Error: fmt::Debug,
        <i64 as TryInto<T>>::Error: fmt::Debug,
        <u64 as TryInto<T>>::Error: fmt::Debug,
    {
        match self {
            Number::Signed(val) => {
                let upper_bound_check = if let Some(upper_bound) = upper_bound {
                    val <= upper_bound.try_into().unwrap()
                } else {
                    true
                };
                if val >= lower_bound.try_into().unwrap() && upper_bound_check {
                    val.try_into()
                        .map_err(|e| DecodeError::InvalidConversion(format!("{:?}", e)))
                } else {
                    Err(DecodeError::InvalidConversion(format!(
                        "{} is out of bounds for conversion",
                        val
                    )))
                }
            }
            Number::Unsigned(val) => {
                let upper_bound_check = if let Some(upper_bound) = upper_bound {
                    val <= upper_bound.try_into().unwrap()
                } else {
                    true
                };

                if upper_bound_check {
                    val.try_into()
                        .map_err(|e| DecodeError::InvalidConversion(format!("{:?}", e)))
                } else {
                    Err(DecodeError::InvalidConversion(format!(
                        "{} is out of bounds for conversion",
                        val
                    )))
                }
            }
            _ => Err(DecodeError::InvalidConversion(
                "Cannot convert float to int".to_owned(),
            )),
        }
    }
}

impl TryFrom<Number> for i8 {
    type Error = DecodeError;
    fn try_from(value: Number) -> Result<Self, Self::Error> {
        value.bounded_int_conversion(i8::MIN, Some(i8::MAX))
    }
}

impl TryFrom<Number> for i32 {
    type Error = DecodeError;
    fn try_from(value: Number) -> Result<Self, Self::Error> {
        value.bounded_int_conversion(i32::MIN, Some(i32::MAX))
    }
}

impl TryFrom<Number> for i64 {
    type Error = DecodeError;
    fn try_from(value: Number) -> Result<Self, Self::Error> {
        value.bounded_int_conversion(i64::MIN, Some(i64::MAX))
    }
}

impl TryFrom<Number> for u8 {
    type Error = DecodeError;
    fn try_from(value: Number) -> Result<Self, Self::Error> {
        value.bounded_int_conversion(u8::MIN, Some(u8::MAX))
    }
}

impl TryFrom<Number> for u32 {
    type Error = DecodeError;
    fn try_from(value: Number) -> Result<Self, Self::Error> {
        value.bounded_int_conversion(u32::MIN, Some(u32::MAX))
    }
}

impl TryFrom<Number> for u64 {
    type Error = DecodeError;
    fn try_from(value: Number) -> Result<Self, Self::Error> {
        value.bounded_int_conversion(u64::MIN, None)
    }
}

impl TryFrom<Number> for f64 {
    type Error = DecodeError;
    fn try_from(value: Number) -> Result<Self, Self::Error> {
        match value {
            Number::Unsigned(val) => {
                if val <= f64::MAX as u64 {
                    Ok(val as f64)
                } else {
                    Err(DecodeError::InvalidConversion(format!(
                        "{} is out of bounds for conversion",
                        val
                    )))
                }
            }
            Number::Signed(val) => {
                if val >= f64::MIN as i64 && val <= f64::MAX as i64 {
                    Ok(val as f64)
                } else {
                    Err(DecodeError::InvalidConversion(format!(
                        "{} is out of bounds for conversion",
                        val
                    )))
                }
            }
            Number::Float(val) => Ok(val),
        }
    }
}

pub fn read_number(buf: &mut &[u8]) -> Result<Number, DecodeError> {
    match rmp::decode::read_marker(buf)
        .map_err(|_| DecodeError::InvalidFormat("Unable to read marker for number".to_owned()))?
    {
        Marker::FixPos(val) => Ok(Number::Unsigned(val as u64)),
        Marker::FixNeg(val) => Ok(Number::Signed(val as i64)),
        Marker::U8 => Ok(Number::Unsigned(
            buf.read_data_u8().map_err(|_| DecodeError::IOError)? as u64,
        )),
        Marker::U16 => Ok(Number::Unsigned(
            buf.read_data_u16().map_err(|_| DecodeError::IOError)? as u64,
        )),
        Marker::U32 => Ok(Number::Unsigned(
            buf.read_data_u32().map_err(|_| DecodeError::IOError)? as u64,
        )),
        Marker::U64 => Ok(Number::Unsigned(
            buf.read_data_u64().map_err(|_| DecodeError::IOError)?,
        )),
        Marker::I8 => Ok(Number::Signed(
            buf.read_data_i8().map_err(|_| DecodeError::IOError)? as i64,
        )),
        Marker::I16 => Ok(Number::Signed(
            buf.read_data_i16().map_err(|_| DecodeError::IOError)? as i64,
        )),
        Marker::I32 => Ok(Number::Signed(
            buf.read_data_i32().map_err(|_| DecodeError::IOError)? as i64,
        )),
        Marker::I64 => Ok(Number::Signed(
            buf.read_data_i64().map_err(|_| DecodeError::IOError)?,
        )),
        Marker::F32 => Ok(Number::Float(
            buf.read_data_f32().map_err(|_| DecodeError::IOError)? as f64,
        )),
        Marker::F64 => Ok(Number::Float(
            buf.read_data_f64().map_err(|_| DecodeError::IOError)?,
        )),
        _ => Err(DecodeError::InvalidType("Invalid number type".to_owned())),
    }
}

pub fn read_number_bytes<T: TryFrom<Number, Error = DecodeError>>(
    buf: &mut Bytes,
) -> Result<T, DecodeError> {
    read_number(unsafe { buf.as_mut_slice() })?.try_into()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::f64;

    #[test]
    fn test_i64_conversions() {
        let valid_max = i64::MAX;
        let valid_unsigned_number = Number::Unsigned(valid_max as u64);
        let zero_unsigned = Number::Unsigned(0u64);
        let zero_signed = Number::Unsigned(0u64);
        let valid_signed_number = Number::Signed(valid_max);
        let invalid_float_number = Number::Float(4.14);
        let invalid_unsigned = u64::MAX;
        let invalid_unsigned_number = Number::Unsigned(invalid_unsigned);

        assert_eq!(
            valid_max,
            TryInto::<i64>::try_into(valid_unsigned_number).unwrap()
        );
        assert_eq!(
            valid_max,
            TryInto::<i64>::try_into(valid_signed_number).unwrap()
        );
        assert_eq!(0, TryInto::<i64>::try_into(zero_signed).unwrap());
        assert_eq!(0, TryInto::<i64>::try_into(zero_unsigned).unwrap());
        assert_eq!(
            Err(DecodeError::InvalidConversion(
                "Cannot convert float to int".to_owned()
            )),
            TryInto::<i64>::try_into(invalid_float_number)
        );
        assert_eq!(
            Err(DecodeError::InvalidConversion(format!(
                "{} is out of bounds for conversion",
                invalid_unsigned
            ))),
            TryInto::<i64>::try_into(invalid_unsigned_number)
        );
    }
    #[test]
    fn test_i32_conversions() {
        let valid_signed_upper = i32::MAX;
        let valid_unsigned_number = Number::Unsigned(valid_signed_upper as u64);
        let zero_unsigned = Number::Unsigned(0u64);
        let zero_signed = Number::Unsigned(0u64);
        let valid_signed_number_upper = Number::Signed(valid_signed_upper as i64);
        let valid_signed_lower = i32::MIN;
        let valid_signed_number_lower = Number::Signed(valid_signed_lower as i64);
        let invalid_float_number = Number::Float(4.14);
        let invalid_unsigned = u64::MAX;
        let invalid_unsigned_number = Number::Unsigned(invalid_unsigned);
        let invalid_signed_upper = i32::MAX as i64 + 1;
        let invalid_signed_number_upper = Number::Signed(invalid_signed_upper);
        let invalid_signed_lower = i32::MIN as i64 - 1;
        let invalid_signed_number_lower = Number::Signed(invalid_signed_lower);

        assert_eq!(
            valid_signed_upper,
            TryInto::<i32>::try_into(valid_unsigned_number).unwrap()
        );
        assert_eq!(
            valid_signed_upper,
            TryInto::<i32>::try_into(valid_signed_number_upper).unwrap()
        );
        assert_eq!(
            valid_signed_lower,
            TryInto::<i32>::try_into(valid_signed_number_lower).unwrap()
        );
        assert_eq!(0, TryInto::<i32>::try_into(zero_signed).unwrap());
        assert_eq!(0, TryInto::<i32>::try_into(zero_unsigned).unwrap());
        assert_eq!(
            Err(DecodeError::InvalidConversion(
                "Cannot convert float to int".to_owned()
            )),
            TryInto::<i32>::try_into(invalid_float_number)
        );
        assert_eq!(
            Err(DecodeError::InvalidConversion(format!(
                "{} is out of bounds for conversion",
                invalid_unsigned
            ))),
            TryInto::<i32>::try_into(invalid_unsigned_number)
        );
        assert_eq!(
            Err(DecodeError::InvalidConversion(format!(
                "{} is out of bounds for conversion",
                invalid_signed_upper
            ))),
            TryInto::<i32>::try_into(invalid_signed_number_upper)
        );
        assert_eq!(
            Err(DecodeError::InvalidConversion(format!(
                "{} is out of bounds for conversion",
                invalid_signed_lower
            ))),
            TryInto::<i32>::try_into(invalid_signed_number_lower)
        );
    }

    #[test]
    fn test_i8_conversions() {
        let valid_signed_upper = i8::MAX;
        let valid_unsigned_number = Number::Unsigned(valid_signed_upper as u64);
        let zero_unsigned = Number::Unsigned(0u64);
        let zero_signed = Number::Unsigned(0u64);
        let valid_signed_number_upper = Number::Signed(valid_signed_upper as i64);
        let valid_signed_lower = i8::MIN;
        let valid_signed_number_lower = Number::Signed(valid_signed_lower as i64);
        let invalid_float_number = Number::Float(4.14);
        let invalid_unsigned = u8::MAX;
        let invalid_unsigned_number = Number::Unsigned(invalid_unsigned as u64);
        let invalid_signed_upper = i8::MAX as i64 + 1;
        let invalid_signed_number_upper = Number::Signed(invalid_signed_upper);
        let invalid_signed_lower = i8::MIN as i64 - 1;
        let invalid_signed_number_lower = Number::Signed(invalid_signed_lower);

        assert_eq!(
            valid_signed_upper,
            TryInto::<i8>::try_into(valid_unsigned_number).unwrap()
        );
        assert_eq!(
            valid_signed_upper,
            TryInto::<i8>::try_into(valid_signed_number_upper).unwrap()
        );
        assert_eq!(
            valid_signed_lower,
            TryInto::<i8>::try_into(valid_signed_number_lower).unwrap()
        );
        assert_eq!(0, TryInto::<i8>::try_into(zero_signed).unwrap());
        assert_eq!(0, TryInto::<i8>::try_into(zero_unsigned).unwrap());
        assert_eq!(
            Err(DecodeError::InvalidConversion(
                "Cannot convert float to int".to_owned()
            )),
            TryInto::<i8>::try_into(invalid_float_number)
        );
        assert_eq!(
            Err(DecodeError::InvalidConversion(format!(
                "{} is out of bounds for conversion",
                invalid_unsigned
            ))),
            TryInto::<i8>::try_into(invalid_unsigned_number)
        );
        assert_eq!(
            Err(DecodeError::InvalidConversion(format!(
                "{} is out of bounds for conversion",
                invalid_signed_upper
            ))),
            TryInto::<i8>::try_into(invalid_signed_number_upper)
        );
        assert_eq!(
            Err(DecodeError::InvalidConversion(format!(
                "{} is out of bounds for conversion",
                invalid_signed_lower
            ))),
            TryInto::<i8>::try_into(invalid_signed_number_lower)
        );
    }

    #[test]
    fn test_u8_conversions() {
        let valid_signed_upper = u8::MAX;
        let valid_unsigned_number = Number::Unsigned(valid_signed_upper as u64);
        let zero_unsigned = Number::Unsigned(0u64);
        let zero_signed = Number::Unsigned(0u64);
        let valid_signed_number_upper = Number::Signed(valid_signed_upper as i64);
        let valid_signed_lower = u8::MIN;
        let valid_signed_number_lower = Number::Signed(valid_signed_lower as i64);
        let invalid_float_number = Number::Float(4.14);
        let invalid_unsigned = (u8::MAX as u64) + 1;
        let invalid_unsigned_number = Number::Unsigned(invalid_unsigned);
        let invalid_signed_upper = i32::MAX as i64 + 1;
        let invalid_signed_number_upper = Number::Signed(invalid_signed_upper);
        let invalid_signed_lower = i8::MIN as i64;
        let invalid_signed_number_lower = Number::Signed(invalid_signed_lower);

        assert_eq!(
            valid_signed_upper,
            TryInto::<u8>::try_into(valid_unsigned_number).unwrap()
        );
        assert_eq!(
            valid_signed_upper,
            TryInto::<u8>::try_into(valid_signed_number_upper).unwrap()
        );
        assert_eq!(
            valid_signed_lower,
            TryInto::<u8>::try_into(valid_signed_number_lower).unwrap()
        );
        assert_eq!(0, TryInto::<u8>::try_into(zero_signed).unwrap());
        assert_eq!(0, TryInto::<u8>::try_into(zero_unsigned).unwrap());
        assert_eq!(
            Err(DecodeError::InvalidConversion(
                "Cannot convert float to int".to_owned()
            )),
            TryInto::<u8>::try_into(invalid_float_number)
        );
        assert_eq!(
            Err(DecodeError::InvalidConversion(format!(
                "{} is out of bounds for conversion",
                invalid_unsigned
            ))),
            TryInto::<u8>::try_into(invalid_unsigned_number)
        );
        assert_eq!(
            Err(DecodeError::InvalidConversion(format!(
                "{} is out of bounds for conversion",
                invalid_signed_upper
            ))),
            TryInto::<u8>::try_into(invalid_signed_number_upper)
        );
        assert_eq!(
            Err(DecodeError::InvalidConversion(format!(
                "{} is out of bounds for conversion",
                invalid_signed_lower
            ))),
            TryInto::<u8>::try_into(invalid_signed_number_lower)
        );
    }

    #[test]
    fn test_u32_conversions() {
        let valid_signed_upper = u32::MAX;
        let valid_unsigned_number = Number::Unsigned(valid_signed_upper as u64);
        let zero_unsigned = Number::Unsigned(0u64);
        let zero_signed = Number::Unsigned(0u64);
        let valid_signed_number_upper = Number::Signed(valid_signed_upper as i64);
        let valid_signed_lower = u32::MIN;
        let valid_signed_number_lower = Number::Signed(valid_signed_lower as i64);
        let invalid_float_number = Number::Float(4.14);
        let invalid_unsigned = (u32::MAX as u64) + 1;
        let invalid_unsigned_number = Number::Unsigned(invalid_unsigned);
        let invalid_signed_upper = i64::MAX;
        let invalid_signed_number_upper = Number::Signed(invalid_signed_upper);
        let invalid_signed_lower = i8::MIN as i64;
        let invalid_signed_number_lower = Number::Signed(invalid_signed_lower);

        assert_eq!(
            valid_signed_upper,
            TryInto::<u32>::try_into(valid_unsigned_number).unwrap()
        );
        assert_eq!(
            valid_signed_upper,
            TryInto::<u32>::try_into(valid_signed_number_upper).unwrap()
        );
        assert_eq!(
            valid_signed_lower,
            TryInto::<u32>::try_into(valid_signed_number_lower).unwrap()
        );
        assert_eq!(0, TryInto::<u32>::try_into(zero_signed).unwrap());
        assert_eq!(0, TryInto::<u32>::try_into(zero_unsigned).unwrap());
        assert_eq!(
            Err(DecodeError::InvalidConversion(
                "Cannot convert float to int".to_owned()
            )),
            TryInto::<u32>::try_into(invalid_float_number)
        );
        assert_eq!(
            Err(DecodeError::InvalidConversion(format!(
                "{} is out of bounds for conversion",
                invalid_unsigned
            ))),
            TryInto::<u32>::try_into(invalid_unsigned_number)
        );
        assert_eq!(
            Err(DecodeError::InvalidConversion(format!(
                "{} is out of bounds for conversion",
                invalid_signed_upper
            ))),
            TryInto::<u32>::try_into(invalid_signed_number_upper)
        );
        assert_eq!(
            Err(DecodeError::InvalidConversion(format!(
                "{} is out of bounds for conversion",
                invalid_signed_lower
            ))),
            TryInto::<u32>::try_into(invalid_signed_number_lower)
        );
    }

    #[test]
    fn test_u64_conversions() {
        let valid_unsigned = u64::MAX;
        let valid_unsigned_number = Number::Unsigned(valid_unsigned);
        let zero_unsigned = Number::Unsigned(0u64);
        let zero_signed = Number::Unsigned(0u64);
        let valid_signed_upper = i64::MAX as u64;
        let valid_signed_number_upper = Number::Signed(valid_signed_upper as i64);
        let valid_signed_lower = u32::MIN as u64;
        let valid_signed_number_lower = Number::Signed(valid_signed_lower as i64);
        let invalid_float_number = Number::Float(4.14);
        let invalid_signed_lower = i8::MIN as i64;
        let invalid_signed_number_lower = Number::Signed(invalid_signed_lower);

        assert_eq!(
            valid_unsigned,
            TryInto::<u64>::try_into(valid_unsigned_number).unwrap()
        );
        assert_eq!(
            valid_signed_upper,
            TryInto::<u64>::try_into(valid_signed_number_upper).unwrap()
        );
        assert_eq!(
            valid_signed_lower,
            TryInto::<u64>::try_into(valid_signed_number_lower).unwrap()
        );
        assert_eq!(0, TryInto::<u64>::try_into(zero_signed).unwrap());
        assert_eq!(0, TryInto::<u64>::try_into(zero_unsigned).unwrap());
        assert_eq!(
            Err(DecodeError::InvalidConversion(
                "Cannot convert float to int".to_owned()
            )),
            TryInto::<u64>::try_into(invalid_float_number)
        );
        assert_eq!(
            Err(DecodeError::InvalidConversion(format!(
                "{} is out of bounds for conversion",
                invalid_signed_lower
            ))),
            TryInto::<u64>::try_into(invalid_signed_number_lower)
        );
    }

    #[test]
    fn test_f64_conversions() {
        let valid_signed_upper = i64::MAX;
        let valid_unsigned_upper = u64::MAX;
        let valid_signed_number_upper = Number::Signed(valid_signed_upper);
        let valid_signed_lower = i64::MIN;
        let valid_signed_number_lower = Number::Signed(valid_signed_lower);
        let valid_unsigned_number = Number::Unsigned(valid_unsigned_upper);
        let zero_unsigned = Number::Unsigned(0u64);
        let zero_signed = Number::Unsigned(0u64);
        let invalid_unsigned = u64::MAX;
        let invalid_unsigned_number = Number::Unsigned(invalid_unsigned);

        assert_eq!(
            valid_unsigned_upper as f64,
            TryInto::<f64>::try_into(valid_unsigned_number).unwrap()
        );
        assert_eq!(
            valid_signed_upper as f64,
            TryInto::<f64>::try_into(valid_signed_number_upper).unwrap()
        );
        assert_eq!(
            valid_signed_lower as f64,
            TryInto::<f64>::try_into(valid_signed_number_lower).unwrap()
        );
        assert_eq!(0f64, TryInto::<f64>::try_into(zero_signed).unwrap());
        assert_eq!(0f64, TryInto::<f64>::try_into(zero_unsigned).unwrap());
        assert_eq!(
            Err(DecodeError::InvalidConversion(format!(
                "{} is out of bounds for conversion",
                invalid_unsigned
            ))),
            TryInto::<i64>::try_into(invalid_unsigned_number)
        );
    }
}
