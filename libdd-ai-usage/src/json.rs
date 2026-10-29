// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! A small JSON reader that keeps what the metric contract needs from a
//! number.
//!
//! `PORTABLE-METRICS.md` pins what happens for a number no double holds
//! (`1e400`) and for a cost stated as a decimal (`1.0050000005`). A reader
//! that fails on the first, or that reads the second one unit in the last
//! place off, cannot give the pinned result. This reader does neither:
//!
//! - a number token is validated against the JSON grammar and then read with `str::parse::<f64>`,
//!   which is correctly rounded;
//! - a token that overflows a double becomes positive or negative infinity, which every projector
//!   treats as "above every bound".
//!
//! Objects are sorted maps, so no result depends on member order. A repeated
//! member keeps its last value.

use std::collections::BTreeMap;
use std::fmt;

/// Nesting deeper than this is refused, so hostile input cannot exhaust the
/// stack.
const MAX_DEPTH: usize = 128;

/// A JSON object with its members in key order.
pub type JsonObject = BTreeMap<String, Json>;

/// A parsed JSON value.
///
/// `Number` holds a finite double, or an infinity for a token too large for
/// a double. It never holds NaN when it came from [`Json::parse`].
#[derive(Debug, Clone, PartialEq)]
pub enum Json {
    Null,
    Bool(bool),
    Number(f64),
    String(String),
    Array(Vec<Json>),
    Object(JsonObject),
}

/// Why a text is not JSON.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JsonErrorKind {
    UnexpectedEnd,
    UnexpectedCharacter,
    InvalidNumber,
    InvalidEscape,
    InvalidUnicode,
    ControlCharacterInString,
    TrailingCharacters,
    TooDeep,
}

impl JsonErrorKind {
    /// A stable identifier for the fault.
    pub fn as_str(self) -> &'static str {
        match self {
            JsonErrorKind::UnexpectedEnd => "unexpected_end",
            JsonErrorKind::UnexpectedCharacter => "unexpected_character",
            JsonErrorKind::InvalidNumber => "invalid_number",
            JsonErrorKind::InvalidEscape => "invalid_escape",
            JsonErrorKind::InvalidUnicode => "invalid_unicode",
            JsonErrorKind::ControlCharacterInString => "control_character_in_string",
            JsonErrorKind::TrailingCharacters => "trailing_characters",
            JsonErrorKind::TooDeep => "too_deep",
        }
    }
}

/// A text that could not be read as JSON, with the byte offset of the fault.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct JsonError {
    pub kind: JsonErrorKind,
    pub offset: usize,
}

impl fmt::Display for JsonError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "invalid JSON at byte {}: {}",
            self.offset,
            self.kind.as_str()
        )
    }
}

impl std::error::Error for JsonError {}

impl Json {
    /// Read one JSON document.
    pub fn parse(text: &str) -> Result<Json, JsonError> {
        let mut reader = Reader {
            bytes: text.as_bytes(),
            text,
            at: 0,
        };
        reader.skip_whitespace();
        let value = reader.value(0)?;
        reader.skip_whitespace();
        if reader.at != reader.bytes.len() {
            return Err(reader.error(JsonErrorKind::TrailingCharacters));
        }
        Ok(value)
    }

    /// The member `key` of an object; `None` for other values.
    pub fn get(&self, key: &str) -> Option<&Json> {
        match self {
            Json::Object(map) => map.get(key),
            _ => None,
        }
    }

    pub fn is_null(&self) -> bool {
        matches!(self, Json::Null)
    }

    pub fn as_str(&self) -> Option<&str> {
        match self {
            Json::String(text) => Some(text),
            _ => None,
        }
    }

    /// The number, which may be an infinity for a token no double holds.
    pub fn as_f64(&self) -> Option<f64> {
        match self {
            Json::Number(number) => Some(*number),
            _ => None,
        }
    }

    pub fn as_bool(&self) -> Option<bool> {
        match self {
            Json::Bool(flag) => Some(*flag),
            _ => None,
        }
    }

    pub fn as_array(&self) -> Option<&[Json]> {
        match self {
            Json::Array(items) => Some(items),
            _ => None,
        }
    }

    pub fn as_object(&self) -> Option<&JsonObject> {
        match self {
            Json::Object(map) => Some(map),
            _ => None,
        }
    }
}

impl From<&str> for Json {
    fn from(text: &str) -> Self {
        Json::String(text.to_string())
    }
}

impl From<bool> for Json {
    fn from(flag: bool) -> Self {
        Json::Bool(flag)
    }
}

impl From<f64> for Json {
    fn from(number: f64) -> Self {
        Json::Number(number)
    }
}

struct Reader<'a> {
    bytes: &'a [u8],
    text: &'a str,
    at: usize,
}

impl Reader<'_> {
    fn error(&self, kind: JsonErrorKind) -> JsonError {
        JsonError {
            kind,
            offset: self.at,
        }
    }

    fn peek(&self) -> Option<u8> {
        self.bytes.get(self.at).copied()
    }

    fn skip_whitespace(&mut self) {
        while matches!(self.peek(), Some(b' ' | b'\t' | b'\n' | b'\r')) {
            self.at += 1;
        }
    }

    fn literal(&mut self, word: &str, value: Json) -> Result<Json, JsonError> {
        let end = self.at.saturating_add(word.len());
        if self.bytes.get(self.at..end) == Some(word.as_bytes()) {
            self.at = end;
            Ok(value)
        } else if end > self.bytes.len() {
            Err(self.error(JsonErrorKind::UnexpectedEnd))
        } else {
            Err(self.error(JsonErrorKind::UnexpectedCharacter))
        }
    }

    fn value(&mut self, depth: usize) -> Result<Json, JsonError> {
        if depth > MAX_DEPTH {
            return Err(self.error(JsonErrorKind::TooDeep));
        }
        match self.peek() {
            None => Err(self.error(JsonErrorKind::UnexpectedEnd)),
            Some(b'n') => self.literal("null", Json::Null),
            Some(b't') => self.literal("true", Json::Bool(true)),
            Some(b'f') => self.literal("false", Json::Bool(false)),
            Some(b'"') => self.string().map(Json::String),
            Some(b'[') => self.array(depth),
            Some(b'{') => self.object(depth),
            Some(b'-' | b'0'..=b'9') => self.number(),
            Some(_) => Err(self.error(JsonErrorKind::UnexpectedCharacter)),
        }
    }

    fn array(&mut self, depth: usize) -> Result<Json, JsonError> {
        self.at += 1;
        let mut items = Vec::new();
        self.skip_whitespace();
        if self.peek() == Some(b']') {
            self.at += 1;
            return Ok(Json::Array(items));
        }
        loop {
            self.skip_whitespace();
            items.push(self.value(depth + 1)?);
            self.skip_whitespace();
            match self.peek() {
                Some(b',') => self.at += 1,
                Some(b']') => {
                    self.at += 1;
                    return Ok(Json::Array(items));
                }
                None => return Err(self.error(JsonErrorKind::UnexpectedEnd)),
                Some(_) => return Err(self.error(JsonErrorKind::UnexpectedCharacter)),
            }
        }
    }

    fn object(&mut self, depth: usize) -> Result<Json, JsonError> {
        self.at += 1;
        let mut map = JsonObject::new();
        self.skip_whitespace();
        if self.peek() == Some(b'}') {
            self.at += 1;
            return Ok(Json::Object(map));
        }
        loop {
            self.skip_whitespace();
            match self.peek() {
                Some(b'"') => {}
                None => return Err(self.error(JsonErrorKind::UnexpectedEnd)),
                Some(_) => return Err(self.error(JsonErrorKind::UnexpectedCharacter)),
            }
            let key = self.string()?;
            self.skip_whitespace();
            match self.peek() {
                Some(b':') => self.at += 1,
                None => return Err(self.error(JsonErrorKind::UnexpectedEnd)),
                Some(_) => return Err(self.error(JsonErrorKind::UnexpectedCharacter)),
            }
            self.skip_whitespace();
            let value = self.value(depth + 1)?;
            map.insert(key, value);
            self.skip_whitespace();
            match self.peek() {
                Some(b',') => self.at += 1,
                Some(b'}') => {
                    self.at += 1;
                    return Ok(Json::Object(map));
                }
                None => return Err(self.error(JsonErrorKind::UnexpectedEnd)),
                Some(_) => return Err(self.error(JsonErrorKind::UnexpectedCharacter)),
            }
        }
    }

    fn digits(&mut self) -> usize {
        let start = self.at;
        while matches!(self.peek(), Some(b'0'..=b'9')) {
            self.at += 1;
        }
        self.at - start
    }

    /// `-? (0 | [1-9][0-9]*) (\. [0-9]+)? ([eE] [+-]? [0-9]+)?`
    fn number(&mut self) -> Result<Json, JsonError> {
        let start = self.at;
        if self.peek() == Some(b'-') {
            self.at += 1;
        }
        match self.peek() {
            Some(b'0') => self.at += 1,
            Some(b'1'..=b'9') => {
                self.digits();
            }
            _ => return Err(self.error(JsonErrorKind::InvalidNumber)),
        }
        if self.peek() == Some(b'.') {
            self.at += 1;
            if self.digits() == 0 {
                return Err(self.error(JsonErrorKind::InvalidNumber));
            }
        }
        if matches!(self.peek(), Some(b'e' | b'E')) {
            self.at += 1;
            if matches!(self.peek(), Some(b'+' | b'-')) {
                self.at += 1;
            }
            if self.digits() == 0 {
                return Err(self.error(JsonErrorKind::InvalidNumber));
            }
        }
        // The token is ASCII, so the slice is on character boundaries. An
        // overflowing token parses to an infinity, never to an error.
        self.text
            .get(start..self.at)
            .and_then(|token| token.parse::<f64>().ok())
            .map(Json::Number)
            .ok_or_else(|| self.error(JsonErrorKind::InvalidNumber))
    }

    fn hex4(&mut self) -> Result<u32, JsonError> {
        let mut unit = 0u32;
        for _ in 0..4 {
            let digit = match self.peek() {
                Some(byte) => char::from(byte)
                    .to_digit(16)
                    .ok_or_else(|| self.error(JsonErrorKind::InvalidEscape))?,
                None => return Err(self.error(JsonErrorKind::UnexpectedEnd)),
            };
            unit = unit * 16 + digit;
            self.at += 1;
        }
        Ok(unit)
    }

    fn escape(&mut self, out: &mut String) -> Result<(), JsonError> {
        let Some(byte) = self.peek() else {
            return Err(self.error(JsonErrorKind::UnexpectedEnd));
        };
        self.at += 1;
        let simple = match byte {
            b'"' => '"',
            b'\\' => '\\',
            b'/' => '/',
            b'b' => '\u{8}',
            b'f' => '\u{c}',
            b'n' => '\n',
            b'r' => '\r',
            b't' => '\t',
            b'u' => {
                let first = self.hex4()?;
                let scalar = if (0xD800..0xDC00).contains(&first) {
                    if self.bytes.get(self.at..self.at.saturating_add(2)) != Some(b"\\u") {
                        return Err(self.error(JsonErrorKind::InvalidUnicode));
                    }
                    self.at += 2;
                    let second = self.hex4()?;
                    if !(0xDC00..0xE000).contains(&second) {
                        return Err(self.error(JsonErrorKind::InvalidUnicode));
                    }
                    0x10000 + ((first - 0xD800) << 10) + (second - 0xDC00)
                } else {
                    first
                };
                char::from_u32(scalar).ok_or_else(|| self.error(JsonErrorKind::InvalidUnicode))?
            }
            _ => {
                self.at -= 1;
                return Err(self.error(JsonErrorKind::InvalidEscape));
            }
        };
        out.push(simple);
        Ok(())
    }

    fn string(&mut self) -> Result<String, JsonError> {
        self.at += 1;
        let mut out = String::new();
        loop {
            let run = self.at;
            while !matches!(self.peek(), None | Some(b'"' | b'\\' | 0..=0x1f)) {
                self.at += 1;
            }
            // The run ends before an ASCII byte, so it is on character
            // boundaries of the input, which is valid UTF-8.
            match self.text.get(run..self.at) {
                Some(chunk) => out.push_str(chunk),
                None => return Err(self.error(JsonErrorKind::InvalidUnicode)),
            }
            match self.peek() {
                None => return Err(self.error(JsonErrorKind::UnexpectedEnd)),
                Some(b'"') => {
                    self.at += 1;
                    return Ok(out);
                }
                Some(b'\\') => {
                    self.at += 1;
                    self.escape(&mut out)?;
                }
                Some(_) => return Err(self.error(JsonErrorKind::ControlCharacterInString)),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn number(text: &str) -> f64 {
        match Json::parse(text) {
            Ok(Json::Number(n)) => n,
            other => panic!("{text}: {other:?}"),
        }
    }

    #[test]
    fn a_number_no_double_holds_is_an_infinity_not_an_error() {
        assert_eq!(number("1e400"), f64::INFINITY);
        assert_eq!(number("-1e400"), f64::NEG_INFINITY);
        assert_eq!(number("1E+400"), f64::INFINITY);
        assert_eq!(number(&"9".repeat(400)), f64::INFINITY);
        assert_eq!(number("1e-400"), 0.0);
    }

    #[test]
    fn integers_beyond_exact_double_range_read_as_the_nearest_double() {
        assert_eq!(number("9007199254740991"), 9_007_199_254_740_991.0);
        assert_eq!(number("9007199254740992"), 9_007_199_254_740_992.0);
        // 2^53 + 1 has no double; it reads as 2^53, which is above the bound.
        assert_eq!(number("9007199254740993"), 9_007_199_254_740_992.0);
        assert_eq!(number("18446744073709551616"), 18_446_744_073_709_551_616.0);
    }

    #[test]
    fn decimals_are_read_correctly_rounded() {
        for text in [
            "1.0050000005",
            "0.0010000005",
            "0.30000000000000004",
            "9007199.26",
            "5e-10",
            "2.1e-05",
            "1000.0",
        ] {
            assert_eq!(
                number(text).to_bits(),
                text.parse::<f64>().unwrap().to_bits(),
                "{text}"
            );
        }
        assert_eq!(number("1000.0"), 1000.0);
        assert_eq!(number("-0.0").to_bits(), (-0.0f64).to_bits());
    }

    #[test]
    fn the_number_grammar_is_json_not_rust() {
        for text in [
            "01", "+1", ".5", "1.", "1e", "1e+", "-", "inf", "NaN", "Infinity", "0x10", "1_000",
            "--1",
        ] {
            assert!(Json::parse(text).is_err(), "{text} accepted");
        }
    }

    #[test]
    fn member_order_does_not_matter_and_the_last_repeat_wins() {
        let a = Json::parse(r#"{"b": 1, "a": 2, "b": 3}"#).unwrap();
        let b = Json::parse(r#"{"a": 2, "b": 3}"#).unwrap();
        assert_eq!(a, b);
        let keys: Vec<&String> = a.as_object().unwrap().keys().collect();
        assert_eq!(keys, ["a", "b"]);
    }

    #[test]
    fn strings_decode_escapes_and_surrogate_pairs() {
        let value = Json::parse(r#""a\né😀\/é""#).unwrap();
        assert_eq!(value.as_str(), Some("a\né😀/é"));
        for text in [
            r#""\ud83d""#,
            r#""\ude00""#,
            r#""\ud83dx""#,
            r#""\x""#,
            "\"\n\"",
            r#""abc"#,
        ] {
            assert!(Json::parse(text).is_err(), "{text} accepted");
        }
    }

    #[test]
    fn structure_errors_are_reported_without_panicking() {
        for text in [
            "",
            " ",
            "{",
            "[",
            "[1,",
            "[1,]",
            "{\"a\"}",
            "{\"a\":}",
            "{a:1}",
            "nul",
            "tru",
            "1 2",
            "[1] x",
            "{\"a\":1,}",
        ] {
            assert!(Json::parse(text).is_err(), "{text:?} accepted");
        }
        let deep = "[".repeat(100_000);
        assert_eq!(Json::parse(&deep).unwrap_err().kind, JsonErrorKind::TooDeep);
    }
}
