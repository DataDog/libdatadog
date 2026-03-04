// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

// Port of Agent's pkg/obfuscate/json_scanner.go.

/// Opcode returned by [`Scanner::step`] for each input char.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Op {
    Continue,     // uninteresting char (inside a literal)
    BeginLiteral, // first char of a string / number / bool / null
    BeginObject,  // '{'
    ObjectKey,    // ':' — object key just finished
    ObjectValue,  // ',' — non-last object value just finished
    EndObject,    // '}' — object closed
    BeginArray,   // '['
    ArrayValue,   // ',' — non-last array element just finished
    EndArray,     // ']' — array closed
    SkipSpace,    // whitespace outside a literal
    End,          // top-level value ended (whitespace between JSON objects)
    Error,        // syntax error
}

/// What kind of composite value we are currently inside.
#[derive(Clone, Copy, PartialEq, Eq)]
enum ParseState {
    ObjectKey,   // object: expecting a key
    ObjectValue, // object: expecting a value (after ':')
    ArrayValue,  // array: expecting an element
}

/// One variant per position in the JSON grammar.
#[rustfmt::skip]
#[derive(Clone, Copy)]
enum State {
    BeginValue,
    BeginValueOrEmpty,  // after '['
    BeginStringOrEmpty, // after '{'
    BeginString,        // after object key:value,
    EndValue,
    EndTop,
    InString,
    InStringEsc,
    InStringEscU,
    InStringEscU1,
    InStringEscU12,
    InStringEscU123,
    // Numbers
    Neg, Num0, Num1, Dot, Dot0, Exp, ExpSign, Exp0,
    // "true"
    T, Tr, Tru,
    // "false"
    F, Fa, Fal, Fals,
    // "null"
    N, Nu, Nul,
    Error,
}

/// A streaming JSON scanner. Feed chars one at a time via [`Scanner::step`];
/// the returned [`Op`] describes the structural significance of each char.
pub(crate) struct Scanner {
    state: State,
    end_top: bool,
    parse_state: Vec<ParseState>,
    err: Option<String>,
    /// Total chars consumed — incremented by the caller before each `step` call.
    position: i64,
}

impl Scanner {
    pub(crate) fn new() -> Self {
        Scanner {
            state: State::BeginValue,
            end_top: false,
            parse_state: Vec::new(),
            err: None,
            position: 0,
        }
    }

    /// Resets the scanner to its initial state (used internally by `EndTop`).
    pub(crate) fn reset(&mut self) {
        self.state = State::BeginValue;
        self.parse_state.clear();
        self.err = None;
        self.end_top = false;
    }

    /// Signals end-of-input. Returns `Op::End` for a complete value, `Op::Error` otherwise.
    pub(crate) fn eof(&mut self) -> Op {
        if self.err.is_some() {
            return Op::Error;
        }
        if self.end_top {
            return Op::End;
        }
        self.step(' ');
        if self.end_top {
            return Op::End;
        }
        if self.err.is_none() {
            self.err = Some(format!(
                "unexpected end of JSON input at char position {}",
                self.position
            ));
        }
        Op::Error
    }

    /// Advances the scanner by one char and returns its structural opcode.
    pub(crate) fn step(&mut self, c: char) -> Op {
        self.position += 1;
        match self.state {
            State::BeginValue => self.begin_value(c),

            State::BeginValueOrEmpty => {
                if is_space(c) {
                    return Op::SkipSpace;
                }
                if c == ']' {
                    return self.end_value(c);
                }
                self.begin_value(c)
            }

            State::BeginStringOrEmpty => {
                if is_space(c) {
                    return Op::SkipSpace;
                }
                if c == '}' {
                    // Empty object: mark last parse state as ObjectValue so
                    // end_value sees a "}" in ObjectValue context.
                    if let Some(ps) = self.parse_state.last_mut() {
                        *ps = ParseState::ObjectValue;
                    }
                    return self.end_value(c);
                }
                self.begin_string(c)
            }

            State::BeginString => self.begin_string(c),
            State::EndValue => self.end_value(c),
            State::EndTop => self.end_top(c),

            State::InString => match c {
                '"' => {
                    self.state = State::EndValue;
                    Op::Continue
                }
                '\\' => {
                    self.state = State::InStringEsc;
                    Op::Continue
                }
                '\x00'..'\x20' => self.error(c, "in string literal"),
                _ => Op::Continue,
            },

            State::InStringEsc => match c {
                'b' | 'f' | 'n' | 'r' | 't' | '\\' | '/' | '"' => {
                    self.state = State::InString;
                    Op::Continue
                }
                'u' => {
                    self.state = State::InStringEscU;
                    Op::Continue
                }
                _ => self.error(c, "in string escape code"),
            },

            // Four hex digits for \uXXXX
            State::InStringEscU => self.hex_digit(c, State::InStringEscU1),
            State::InStringEscU1 => self.hex_digit(c, State::InStringEscU12),
            State::InStringEscU12 => self.hex_digit(c, State::InStringEscU123),
            State::InStringEscU123 => self.hex_digit(c, State::InString),

            State::Neg => {
                if c == '0' {
                    self.state = State::Num0;
                    Op::Continue
                } else if ('1'..='9').contains(&c) {
                    self.state = State::Num1;
                    Op::Continue
                } else {
                    self.error(c, "in numeric literal")
                }
            }

            // Non-zero integer: keep consuming digits, then fall through to Num0 logic.
            State::Num1 => {
                if c.is_ascii_digit() {
                    Op::Continue
                } else {
                    self.num0(c)
                }
            }

            State::Num0 => self.num0(c),

            State::Dot => {
                if c.is_ascii_digit() {
                    self.state = State::Dot0;
                    Op::Continue
                } else {
                    self.error(c, "after decimal point in numeric literal")
                }
            }

            State::Dot0 => {
                if c.is_ascii_digit() {
                    Op::Continue
                } else if c == 'e' || c == 'E' {
                    self.state = State::Exp;
                    Op::Continue
                } else {
                    self.end_value(c)
                }
            }

            State::Exp => {
                if c == '+' || c == '-' {
                    self.state = State::ExpSign;
                    Op::Continue
                } else {
                    self.exp_sign(c)
                }
            }

            State::ExpSign => self.exp_sign(c),

            State::Exp0 => {
                if c.is_ascii_digit() {
                    Op::Continue
                } else {
                    self.end_value(c)
                }
            }

            // Literal keywords: "true", "false", "null"
            State::T => self.lit(c, 'r', State::Tr, "in literal true (expecting 'r')"),
            State::Tr => self.lit(c, 'u', State::Tru, "in literal true (expecting 'u')"),
            State::Tru => self.lit_end(c, 'e', "in literal true (expecting 'e')"),
            State::F => self.lit(c, 'a', State::Fa, "in literal false (expecting 'a')"),
            State::Fa => self.lit(c, 'l', State::Fal, "in literal false (expecting 'l')"),
            State::Fal => self.lit(c, 's', State::Fals, "in literal false (expecting 's')"),
            State::Fals => self.lit_end(c, 'e', "in literal false (expecting 'e')"),
            State::N => self.lit(c, 'u', State::Nu, "in literal null (expecting 'u')"),
            State::Nu => self.lit(c, 'l', State::Nul, "in literal null (expecting 'l')"),
            State::Nul => self.lit_end(c, 'l', "in literal null (expecting 'l')"),

            State::Error => Op::Error,
        }
    }

    // --- Helper methods ---

    fn begin_value(&mut self, c: char) -> Op {
        if is_space(c) {
            return Op::SkipSpace;
        }
        match c {
            '{' => {
                self.state = State::BeginStringOrEmpty;
                self.parse_state.push(ParseState::ObjectKey);
                Op::BeginObject
            }
            '[' => {
                self.state = State::BeginValueOrEmpty;
                self.parse_state.push(ParseState::ArrayValue);
                Op::BeginArray
            }
            '"' => {
                self.state = State::InString;
                Op::BeginLiteral
            }
            '-' => {
                self.state = State::Neg;
                Op::BeginLiteral
            }
            '0' => {
                self.state = State::Num0;
                Op::BeginLiteral
            }
            't' => {
                self.state = State::T;
                Op::BeginLiteral
            }
            'f' => {
                self.state = State::F;
                Op::BeginLiteral
            }
            'n' => {
                self.state = State::N;
                Op::BeginLiteral
            }
            '1'..='9' => {
                self.state = State::Num1;
                Op::BeginLiteral
            }
            _ => self.error(c, "looking for beginning of value"),
        }
    }

    fn begin_string(&mut self, c: char) -> Op {
        if is_space(c) {
            return Op::SkipSpace;
        }
        if c == '"' {
            self.state = State::InString;
            Op::BeginLiteral
        } else {
            self.error(c, "looking for beginning of object key string")
        }
    }

    fn end_value(&mut self, c: char) -> Op {
        let n = self.parse_state.len();
        if n == 0 {
            self.state = State::EndTop;
            self.end_top = true;
            return self.end_top(c);
        }
        if is_space(c) {
            self.state = State::EndValue;
            return Op::SkipSpace;
        }
        match self.parse_state[n - 1] {
            ParseState::ObjectKey => {
                if c == ':' {
                    self.parse_state[n - 1] = ParseState::ObjectValue;
                    self.state = State::BeginValue;
                    Op::ObjectKey
                } else {
                    self.error(c, "after object key")
                }
            }
            ParseState::ObjectValue => {
                if c == ',' {
                    self.parse_state[n - 1] = ParseState::ObjectKey;
                    self.state = State::BeginString;
                    Op::ObjectValue
                } else if c == '}' {
                    self.pop_parse_state();
                    Op::EndObject
                } else {
                    self.error(c, "after object key:value pair")
                }
            }
            ParseState::ArrayValue => {
                if c == ',' {
                    self.state = State::BeginValue;
                    Op::ArrayValue
                } else if c == ']' {
                    self.pop_parse_state();
                    Op::EndArray
                } else {
                    self.error(c, "after array element")
                }
            }
        }
    }

    fn end_top(&mut self, c: char) -> Op {
        if !is_space(c) {
            // A new JSON value is starting. Reset and process this char fresh.
            // This allows multiple concatenated JSON objects (ElasticSearch bulk API).
            self.reset();
            self.step(c)
        } else {
            Op::End
        }
    }

    fn pop_parse_state(&mut self) {
        let n = self.parse_state.len();
        if n <= 1 {
            self.state = State::EndTop;
            self.end_top = true;
        } else {
            self.parse_state.truncate(n - 1);
            self.state = State::EndValue;
        }
    }

    /// After a decimal point: consume digits, optional exponent, then end value.
    fn num0(&mut self, c: char) -> Op {
        match c {
            '.' => {
                self.state = State::Dot;
                Op::Continue
            }
            'e' | 'E' => {
                self.state = State::Exp;
                Op::Continue
            }
            _ => self.end_value(c),
        }
    }

    fn exp_sign(&mut self, c: char) -> Op {
        if c.is_ascii_digit() {
            self.state = State::Exp0;
            Op::Continue
        } else {
            self.error(c, "in exponent of numeric literal")
        }
    }

    /// One hex digit in a `\uXXXX` escape; on success transitions to `next`.
    fn hex_digit(&mut self, c: char, next: State) -> Op {
        if c.is_ascii_hexdigit() {
            self.state = next;
            Op::Continue
        } else {
            self.error(c, "in \\u hexadecimal character escape")
        }
    }

    /// One character in a keyword literal (true/false/null); on match transitions to `next`.
    fn lit(&mut self, c: char, expected: char, next: State, ctx: &'static str) -> Op {
        if c == expected {
            self.state = next;
            Op::Continue
        } else {
            self.error(c, ctx)
        }
    }

    /// Last character in a keyword literal; on match transitions to `EndValue`.
    fn lit_end(&mut self, c: char, expected: char, ctx: &'static str) -> Op {
        if c == expected {
            self.state = State::EndValue;
            Op::Continue
        } else {
            self.error(c, ctx)
        }
    }

    fn error(&mut self, c: char, ctx: &str) -> Op {
        self.state = State::Error;
        self.err = Some(format!("invalid character '{}' {}", c, ctx));
        Op::Error
    }
}

#[inline]
fn is_space(c: char) -> bool {
    matches!(c, ' ' | '\t' | '\r' | '\n')
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_valid_empty_object() {
        let mut s = Scanner::new();
        for c in "{}".chars() {
            s.position += 1;
            assert_ne!(s.step(c), Op::Error, "error on char '{}'", c as char);
        }
        assert_eq!(s.eof(), Op::End);
    }

    #[test]
    fn test_valid_nested_json() {
        let mut s = Scanner::new();
        for c in r#"{"key":"value","num":42}"#.chars() {
            s.position += 1;
            assert_ne!(s.step(c), Op::Error, "error on char '{}'", c as char);
        }
        assert_eq!(s.eof(), Op::End);
    }

    #[test]
    fn test_truncated_input_returns_error_on_eof() {
        let mut s = Scanner::new();
        for c in r#"{"key":"#.chars() {
            s.position += 1;
            s.step(c);
        }
        assert_eq!(s.eof(), Op::Error);
    }

    #[test]
    fn test_invalid_input_returns_error() {
        let mut s = Scanner::new();
        s.position += 1;
        assert_eq!(s.step(')'), Op::Error);
    }

    #[test]
    fn test_multiple_json_objects_no_errors() {
        let mut s = Scanner::new();
        for c in r#"{"a":1} {"b":2}"#.chars() {
            s.position += 1;
            assert_ne!(s.step(c), Op::Error, "error on char '{}'", c as char);
        }
    }
}
