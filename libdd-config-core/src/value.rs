// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use indexmap::IndexMap;
use serde::{Deserialize, Serialize, de::IgnoredAny};

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ValueKind {
    Boolean,
    #[serde(rename = "int")]
    Integer,
    #[serde(alias = "float")]
    Decimal,
    String,
    Array,
    Map,
    Json,
    Host,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(tag = "kind", content = "value", rename_all = "snake_case")]
pub enum Value {
    Boolean(bool),
    Number(Number),
    String(String),
    Array(Vec<Self>),
    Map(IndexMap<String, String>),
    /// JSON text that the resolver validates before publishing the snapshot.
    Json(String),
    /// An index in the calling host's table of objects and functions.
    Host(u32),
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Serialize)]
#[serde(untagged)]
pub enum Number {
    Integer(i64),
    Float(f64),
    Infinite(Infinity),
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub enum Infinity {
    #[serde(rename = "Infinity")]
    Positive,
    #[serde(rename = "-Infinity")]
    Negative,
}

impl Number {
    #[must_use]
    pub const fn from_float(value: f64) -> Option<Self> {
        if value.is_finite() {
            Some(Self::Float(value))
        } else if value.is_infinite() {
            Some(Self::Infinite(if value.is_sign_negative() {
                Infinity::Negative
            } else {
                Infinity::Positive
            }))
        } else {
            None
        }
    }

    /// Convert the value to binary64, rounding integers as needed.
    #[must_use]
    #[allow(clippy::cast_precision_loss)]
    pub const fn as_float(self) -> f64 {
        match self {
            Self::Integer(value) => value as f64,
            Self::Float(value) => value,
            Self::Infinite(Infinity::Positive) => f64::INFINITY,
            Self::Infinite(Infinity::Negative) => f64::NEG_INFINITY,
        }
    }
}

impl Value {
    #[must_use]
    pub const fn as_bool(&self) -> Option<bool> {
        match self {
            Self::Boolean(value) => Some(*value),
            _ => None,
        }
    }

    #[must_use]
    pub fn as_str(&self) -> Option<&str> {
        match self {
            Self::String(value) => Some(value),
            _ => None,
        }
    }

    pub(crate) fn matches(&self, kind: ValueKind) -> bool {
        if let Self::Number(Number::Float(value)) = self {
            return value.is_finite()
                && (kind == ValueKind::Decimal
                    || kind == ValueKind::Integer && value.fract() == 0.0);
        }
        matches!(
            (kind, self),
            (ValueKind::Boolean, Self::Boolean(_))
                | (ValueKind::Integer | ValueKind::Decimal, Self::Number(_))
                | (ValueKind::String, Self::String(_))
                | (ValueKind::Array, Self::Array(_))
                | (ValueKind::Map, Self::Map(_))
                | (ValueKind::Json, Self::Json(_))
                | (ValueKind::Host, Self::Host(_))
        ) && self.has_valid_representation()
    }

    fn has_valid_representation(&self) -> bool {
        match self {
            Self::Json(value) => valid_json(value),
            Self::Number(Number::Float(value)) => value.is_finite(),
            Self::Array(items) => items.iter().all(Self::has_valid_representation),
            _ => true,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum NumberSyntax {
    #[default]
    Native,
    EcmaScript,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum BooleanSyntax {
    #[default]
    TrueFalse,
    Go,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum MapSyntax {
    #[default]
    Colon,
    ColonOrSpaceSeparated,
    Equals,
    EqualsPercentDecoded,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize)]
#[serde(default)]
pub struct ParsePolicy {
    pub numbers: NumberSyntax,
    pub booleans: BooleanSyntax,
    pub coerce_code: bool,
}

#[derive(Clone, Copy, Debug, Default, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct ParseOptions {
    pub(crate) preserve_empty_items: bool,
    pub(crate) map_syntax: MapSyntax,
}

impl ParsePolicy {
    pub(crate) fn parse(
        self,
        kind: ValueKind,
        options: ParseOptions,
        input: &str,
    ) -> Option<Value> {
        let value = match kind {
            ValueKind::Boolean => Value::Boolean(parse_bool(input, self.booleans)?),
            ValueKind::Integer | ValueKind::Decimal => Value::Number(parse_number(
                input,
                kind == ValueKind::Integer,
                self.numbers,
            )?),
            ValueKind::String => Value::String(input.to_owned()),
            ValueKind::Array => Value::Array(if input.is_empty() {
                Vec::new()
            } else {
                input
                    .split(',')
                    .map(trim)
                    .filter(|part| options.preserve_empty_items || !part.is_empty())
                    .map(|part| Value::String(part.to_owned()))
                    .collect()
            }),
            ValueKind::Map => Value::Map(parse_map(input, options.map_syntax)?),
            ValueKind::Json if valid_json(input) => Value::Json(input.to_owned()),
            ValueKind::Json | ValueKind::Host => return None,
        };
        Some(value)
    }

    pub(crate) fn coerce(
        self,
        kind: ValueKind,
        options: ParseOptions,
        input: Value,
    ) -> Option<Value> {
        if kind == ValueKind::Integer
            && let Value::Number(Number::Float(value)) = input
        {
            return Number::from_float(value.trunc()).map(Value::Number);
        }

        if input.matches(kind) {
            return Some(input);
        }

        if !self.coerce_code {
            return None;
        }

        match (kind, input) {
            (ValueKind::Boolean, Value::String(input)) => self.parse(kind, options, &input),
            (ValueKind::Integer | ValueKind::Decimal, Value::String(input))
                if !trim(&input).is_empty() =>
            {
                self.parse(kind, options, &input)
            }
            (ValueKind::String, Value::Boolean(input)) => Some(Value::String(input.to_string())),
            (ValueKind::String, Value::Number(input)) if input.as_float().is_finite() => {
                Some(Value::String(number_text(input, self.numbers)))
            }
            _ => None,
        }
    }

    pub(crate) fn pattern_matches(self, pattern: &libdd_regex::Regex, value: &Value) -> bool {
        match value {
            Value::String(value) => pattern.is_match(value),
            Value::Boolean(true) => pattern.is_match("true"),
            Value::Boolean(false) => pattern.is_match("false"),
            Value::Number(value) => pattern.is_match(&number_text(*value, self.numbers)),
            _ => false,
        }
    }
}

fn valid_json(value: &str) -> bool {
    serde_json::from_str::<IgnoredAny>(value).is_ok()
}

fn parse_bool(value: &str, syntax: BooleanSyntax) -> Option<bool> {
    match syntax {
        BooleanSyntax::TrueFalse if value == "1" || value.eq_ignore_ascii_case("true") => {
            Some(true)
        }
        BooleanSyntax::TrueFalse if value == "0" || value.eq_ignore_ascii_case("false") => {
            Some(false)
        }
        BooleanSyntax::Go => match value {
            "1" | "t" | "T" | "true" | "TRUE" | "True" => Some(true),
            "0" | "f" | "F" | "false" | "FALSE" | "False" => Some(false),
            _ => None,
        },
        BooleanSyntax::TrueFalse => None,
    }
}

fn parse_number(value: &str, integer: bool, syntax: NumberSyntax) -> Option<Number> {
    let value = match syntax {
        NumberSyntax::Native if integer => return value.parse().ok().map(Number::Integer),
        NumberSyntax::Native => value.parse::<f64>().ok()?,
        NumberSyntax::EcmaScript => parse_ecmascript_number(trim(value))?,
    };
    Number::from_float(if integer { value.trunc() } else { value })
}

fn parse_ecmascript_number(value: &str) -> Option<f64> {
    match value {
        "" => return Some(0.0),
        "Infinity" | "+Infinity" => return Some(f64::INFINITY),
        "-Infinity" => return Some(f64::NEG_INFINITY),
        _ => {}
    }

    if let Some((prefix, digits)) = value.split_at_checked(2) {
        let radix = match prefix {
            "0x" | "0X" => Some(16),
            "0o" | "0O" => Some(8),
            "0b" | "0B" => Some(2),
            _ => None,
        };
        if let Some(radix) = radix {
            if digits.is_empty() {
                return None;
            }
            return parse_radix_integer(digits, radix);
        }
    }

    if value
        .bytes()
        .all(|byte| matches!(byte, b'0'..=b'9' | b'.' | b'+' | b'-' | b'e' | b'E'))
    {
        value.parse().ok()
    } else {
        None
    }
}

#[allow(
    clippy::cast_precision_loss,
    reason = "The rounded significand fits in binary64"
)]
fn parse_radix_integer(digits: &str, radix: u32) -> Option<f64> {
    let digit_width = radix.trailing_zeros();
    let mut significand = 0_u64;
    let mut bit_length = 0_u32;
    let mut sticky = false;
    for character in digits.chars() {
        let digit = character.to_digit(radix)?;
        let width = if bit_length == 0 {
            u32::BITS - digit.leading_zeros()
        } else {
            digit_width
        };
        // Keep the significand and its guard bit. The remaining bits affect ties.
        let kept = 54_u32.saturating_sub(bit_length).min(width);
        let discarded = width - kept;
        significand = (significand << kept) | u64::from(digit >> discarded);
        sticky |= digit & ((1 << discarded) - 1) != 0;
        bit_length = bit_length.saturating_add(width);
    }

    if bit_length <= 53 {
        return Some(significand as f64);
    }
    if bit_length > 1024 {
        return Some(f64::INFINITY);
    }

    let mut rounded = significand >> 1;
    if significand & 1 != 0 && (sticky || rounded & 1 != 0) {
        rounded += 1;
    }
    let scale = f64::from_bits(u64::from(bit_length + 1023 - 53) << 52);
    Some(rounded as f64 * scale)
}

fn number_text(value: Number, syntax: NumberSyntax) -> String {
    match value {
        Number::Integer(value) => value.to_string(),
        Number::Infinite(Infinity::Positive) => "Infinity".to_owned(),
        Number::Infinite(Infinity::Negative) => "-Infinity".to_owned(),
        Number::Float(value) if matches!(syntax, NumberSyntax::EcmaScript) => {
            let magnitude = value.abs();
            if magnitude == 0.0 {
                "0".to_owned()
            } else if !(1e-6..1e21).contains(&magnitude) {
                let mut text = format!("{value:e}");
                if let Some(exponent) = text.find('e')
                    && text.as_bytes()[exponent + 1] != b'-'
                {
                    text.insert(exponent + 1, '+');
                }
                text
            } else {
                value.to_string()
            }
        }
        Number::Float(value) => value.to_string(),
    }
}

fn trim(value: &str) -> &str {
    value.trim_matches(trim_char)
}

fn parse_map(input: &str, syntax: MapSyntax) -> Option<IndexMap<String, String>> {
    let (separator, split_spaces, decode) = match syntax {
        MapSyntax::Colon => (':', false, false),
        MapSyntax::ColonOrSpaceSeparated => (':', !input.contains(','), false),
        MapSyntax::Equals => ('=', false, false),
        MapSyntax::EqualsPercentDecoded => ('=', false, true),
    };

    let mut entries = IndexMap::new();
    for entry in input.split(|character| character == ',' || (split_spaces && trim_char(character)))
    {
        let (key, value) = entry.split_once(separator).unwrap_or((entry, ""));
        let key = trim(key);
        if !key.is_empty() {
            let value = trim(value);
            if decode {
                entries.insert(decode_component(key)?, decode_component(value)?);
            } else {
                entries.insert(key.to_owned(), value.to_owned());
            }
        }
    }
    Some(entries)
}

const fn trim_char(character: char) -> bool {
    character == '\u{feff}' || (character.is_whitespace() && character != '\u{0085}')
}

fn decode_component(value: &str) -> Option<String> {
    if !value.contains('%') {
        return Some(value.to_owned());
    }

    let mut decoded = Vec::with_capacity(value.len());
    let mut bytes = value.as_bytes().iter();
    while let Some(&byte) = bytes.next() {
        if byte == b'%' {
            let high = hex_digit(*bytes.next()?)?;
            let low = hex_digit(*bytes.next()?)?;
            decoded.push((high << 4) | low);
        } else {
            decoded.push(byte);
        }
    }
    String::from_utf8(decoded).ok()
}

const fn hex_digit(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}
