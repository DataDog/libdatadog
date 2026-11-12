// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! Lookups over [`Attributes`], ported from the Agent's `pkg/trace/semantics/lookup.go`.
//!
//! Each lookup walks the entries of a concept in precedence order, skips the ones whose
//! conditions don't hold, and returns the first value it can read with the entry's type. Strings
//! are parsed and numbers formatted like Go's `strconv` does.
//!
//! One known difference with the Agent comes from the attribute model rather than the registry:
//! [`Attributes::attribute_f64`] also returns integers, so a `float64` entry matches an integer
//! attribute on a V1 span, where the Agent's V1 accessor only matches doubles. On v0.4 spans,
//! numbers are all in `metrics` and both behave the same.

use alloc::borrow::Cow;
use alloc::string::{String, ToString};

use libdd_trace_model::{Attributes, f64_to_exact_i64};

use crate::{Concept, Condition, Entry, ValueType};

/// The first value of `concept` as a string, `None` if no entry holds a value.
///
/// String values are borrowed, and empty strings are skipped. Numeric values of `int64` and
/// `float64` entries are formatted like the Agent does (Go's `%d` and `%g`).
///
/// Promoted tags can't be read with lookups: use the `Span` and `TraceChunk` getters instead.
#[must_use]
pub fn lookup_str<A: Attributes + ?Sized>(attrs: &A, concept: Concept) -> Option<Cow<'_, str>> {
    str_in(attrs, concept.entries())
}

/// The first value of `concept` as an integer, `None` if no entry holds one.
///
/// Strings are parsed as integers, then as floats. Floats that are fractional, not finite, or out
/// of range are skipped, and the lookup moves on to the next entry.
///
/// Promoted tags can't be read with lookups: use the `Span` and `TraceChunk` getters instead.
#[must_use]
pub fn lookup_i64<A: Attributes + ?Sized>(attrs: &A, concept: Concept) -> Option<i64> {
    i64_in(attrs, concept.entries())
}

/// The first value of `concept` as a float, `None` if no entry holds one.
///
/// Integers are converted, and strings are parsed. Like the Agent, the strings `NaN`, `Inf` and
/// `Infinity` (any case, the last two with an optional sign) are returned as is, while strings
/// out of the `f64` range are skipped.
///
/// Promoted tags can't be read with lookups: use the `Span` and `TraceChunk` getters instead.
#[must_use]
pub fn lookup_f64<A: Attributes + ?Sized>(attrs: &A, concept: Concept) -> Option<f64> {
    f64_in(attrs, concept.entries())
}

fn str_in<'a, A: Attributes + ?Sized>(attrs: &'a A, entries: &[Entry]) -> Option<Cow<'a, str>> {
    matching(attrs, entries).find_map(|entry| match entry.value_type {
        ValueType::Int64 => attrs
            .attribute_i64(entry.name)
            .map(|v| Cow::Owned(v.to_string())),
        ValueType::Float64 => attrs
            .attribute_f64(entry.name)
            .map(|v| Cow::Owned(format_go_g(v))),
        ValueType::String => non_empty_str(attrs, entry.name).map(Cow::Borrowed),
    })
}

fn i64_in<A: Attributes + ?Sized>(attrs: &A, entries: &[Entry]) -> Option<i64> {
    matching(attrs, entries).find_map(|entry| match entry.value_type {
        // `attribute_i64` is `attribute_f64` + `f64_to_exact_i64`, except that it stays exact for
        // integers above 2^53.
        ValueType::Int64 | ValueType::Float64 => attrs.attribute_i64(entry.name),
        ValueType::String => {
            let v = non_empty_str(attrs, entry.name)?;
            v.parse::<i64>()
                .ok()
                .or_else(|| parse_go_float(v).and_then(f64_to_exact_i64))
        }
    })
}

fn f64_in<A: Attributes + ?Sized>(attrs: &A, entries: &[Entry]) -> Option<f64> {
    matching(attrs, entries).find_map(|entry| match entry.value_type {
        ValueType::Float64 => attrs.attribute_f64(entry.name),
        // Lossy above 2^53, like the Agent's `float64(v)`.
        #[allow(clippy::cast_precision_loss)]
        ValueType::Int64 => attrs.attribute_i64(entry.name).map(|v| v as f64),
        ValueType::String => parse_go_float(non_empty_str(attrs, entry.name)?),
    })
}

fn matching<'e, A: Attributes + ?Sized>(
    attrs: &A,
    entries: &'e [Entry],
) -> impl Iterator<Item = &'e Entry> {
    entries
        .iter()
        .filter(move |entry| entry.when.iter().all(|c| condition_holds(attrs, c)))
}

fn non_empty_str<'a, A: Attributes + ?Sized>(attrs: &'a A, key: &str) -> Option<&'a str> {
    attrs.attribute_str(key).filter(|v| !v.is_empty())
}

/// Conditions read the exact attribute, as a string. An empty string counts as absent.
fn condition_holds<A: Attributes + ?Sized>(attrs: &A, condition: &Condition) -> bool {
    let value = non_empty_str(attrs, condition.attribute);
    if condition
        .present
        .is_some_and(|present| present != value.is_some())
    {
        return false;
    }
    condition.eq.is_none_or(|eq| value == Some(eq))
}

/// Parses `s` like Go's `strconv.ParseFloat(s, 64)`, `None` where it returns an error.
///
/// Go accepts decimal and hexadecimal (`0x1p-2`) literals, `_` between digits, `NaN` without a
/// sign and `Inf` / `Infinity` with an optional sign, all case-insensitive. It rejects values out
/// of range, but not values that round to zero.
fn parse_go_float(s: &str) -> Option<f64> {
    let (negative, unsigned) = match s.as_bytes().first()? {
        b'-' => (true, &s[1..]),
        b'+' => (false, &s[1..]),
        _ => (false, s),
    };
    // Rust's parser would take a second sign.
    if unsigned.starts_with(['+', '-']) {
        return None;
    }
    let signed = |v: f64| if negative { -v } else { v };

    if unsigned.eq_ignore_ascii_case("inf") || unsigned.eq_ignore_ascii_case("infinity") {
        return Some(signed(f64::INFINITY));
    }
    if unsigned.eq_ignore_ascii_case("nan") {
        return (unsigned.len() == s.len()).then_some(f64::NAN);
    }

    let hex = unsigned
        .get(..2)
        .is_some_and(|p| p.eq_ignore_ascii_case("0x"));
    let v = if hex {
        parse_hex_float(&strip_underscores(unsigned, true)?[2..])?
    } else {
        let digits = strip_underscores(unsigned, false)?;
        // Rust's parser takes the same decimal syntax, but also `inf` and `nan`, handled above.
        if !digits
            .bytes()
            .all(|c| c.is_ascii_digit() || matches!(c, b'.' | b'e' | b'E' | b'+' | b'-'))
        {
            return None;
        }
        digits.parse::<f64>().ok()?
    };
    v.is_finite().then(|| signed(v))
}

/// `s` without its underscores, `None` if one is not between two digits (or between the `0x`
/// prefix and a digit), as Go requires.
fn strip_underscores(s: &str, hex: bool) -> Option<Cow<'_, str>> {
    if !s.contains('_') {
        return Some(Cow::Borrowed(s));
    }
    let is_digit = |c: u8| {
        if hex {
            c.is_ascii_hexdigit()
        } else {
            c.is_ascii_digit()
        }
    };
    let bytes = s.as_bytes();
    for (i, _) in s.match_indices('_') {
        let after_digit = i
            .checked_sub(1)
            .and_then(|p| bytes.get(p))
            .is_some_and(|&c| is_digit(c) || (hex && i == 2));
        let before_digit = bytes.get(i + 1).is_some_and(|&c| is_digit(c));
        if !after_digit || !before_digit {
            return None;
        }
    }
    Some(Cow::Owned(s.replace('_', "")))
}

/// Parses the part of a Go hexadecimal float after `0x`: hex digits with an optional point, then a
/// mandatory binary exponent, e.g. `1.8p3`.
fn parse_hex_float(s: &str) -> Option<f64> {
    let (mantissa, exp) = s.split_once(['p', 'P'])?;
    let (int, frac) = mantissa.split_once('.').unwrap_or((mantissa, ""));
    if int.is_empty() && frac.is_empty() {
        return None;
    }
    let exp_digits = exp.strip_prefix(['+', '-']).unwrap_or(exp);
    if exp_digits.is_empty() || !exp_digits.bytes().all(|c| c.is_ascii_digit()) {
        return None;
    }
    // Saturate: anything this far out is zero or out of range anyway.
    let mut exp2 = exp.parse::<i64>().unwrap_or_else(|_| {
        if exp.starts_with('-') {
            i64::MIN / 2
        } else {
            i64::MAX / 2
        }
    });

    // Take digits until the mantissa holds more than 60 bits (at most 64), and remember whether any
    // dropped digit was non-zero so that the rounding below stays correct.
    let mut bits: u64 = 0;
    let mut sticky = false;
    for (i, c) in int.bytes().chain(frac.bytes()).enumerate() {
        let digit = u64::from(char::from(c).to_digit(16)?);
        let in_frac = i >= int.len();
        if bits >> 60 == 0 {
            bits = (bits << 4) | digit;
            if in_frac {
                exp2 = exp2.saturating_sub(4);
            }
        } else {
            sticky |= digit != 0;
            if !in_frac {
                exp2 = exp2.saturating_add(4);
            }
        }
    }
    if bits == 0 {
        return Some(0.0);
    }
    if sticky {
        bits |= 1;
    }

    // The value is `bits * 2^exp2`. Below the normal range, f64 only has bits down to 2^-1074, so
    // round to that precision here, in integers: rounding to 53 bits first and then again when
    // scaling would round twice.
    let top_bit = i64::from(63 - bits.leading_zeros());
    if top_bit.saturating_add(exp2) < -1022 {
        let shift = (-1074_i64).saturating_sub(exp2);
        if shift > 0 {
            let Ok(shift @ 0..=127) = u32::try_from(shift) else {
                return Some(0.0);
            };
            let wide = u128::from(bits);
            let kept = wide >> shift;
            let rest = wide & ((1 << shift) - 1);
            let half = 1 << (shift - 1);
            let round_up = rest > half || (rest == half && kept & 1 == 1);
            // `kept` is below 2^53, since the value is below 2^-1022.
            bits = u64::try_from(kept + u128::from(round_up)).ok()?;
            exp2 = -1074;
        }
    }
    // Rounds to the nearest f64, ties to even. With `sticky` in the lowest bit, which is below the
    // 53 kept bits, ties are only ties if they are exact. Subnormal values have at most 53 bits
    // here, so this is exact for them.
    #[allow(clippy::cast_precision_loss)]
    let v = bits as f64;
    Some(scale_by_pow2(v, exp2))
}

/// `v * 2^exp`, saturating to infinity or zero.
fn scale_by_pow2(mut v: f64, mut exp: i64) -> f64 {
    // 2^±1000 are normal floats, and every step but the last is exact unless the result is out of
    // range or subnormal.
    while exp > 1000 && v.is_finite() {
        v *= pow2(1000);
        exp -= 1000;
    }
    while exp < -1000 && v != 0.0 {
        v *= pow2(-1000);
        exp += 1000;
    }
    v * pow2(exp.clamp(-1000, 1000))
}

/// `2^exp` for `exp` within the normal range, built from its bits to stay exact.
fn pow2(exp: i64) -> f64 {
    u64::try_from(exp + 1023).map_or(0.0, |biased| f64::from_bits(biased << 52))
}

/// Formats `v` like Go's `strconv.FormatFloat(v, 'g', -1, 64)`, which is what `%g` does: the
/// shortest digits that round-trip, in exponent form when the exponent is below -4 or at least 6.
fn format_go_g(v: f64) -> String {
    if v.is_nan() {
        return "NaN".to_owned();
    }
    if v.is_infinite() {
        return if v > 0.0 { "+Inf" } else { "-Inf" }.to_owned();
    }
    if v == 0.0 {
        return if v.is_sign_negative() { "-0" } else { "0" }.to_owned();
    }

    // Rust's `{:e}` gives the shortest digit count that round-trips, e.g. `-1.5e-7` or `1e6`. When
    // two values with that many digits round-trip, Go picks the closest one, ties to even, which
    // is what `{:.Ne}` prints. Rust's shortest output can pick the other one.
    let shortest = format!("{v:e}");
    let digit_count = shortest
        .split_once('e')
        .map_or(0, |(m, _)| m.bytes().filter(u8::is_ascii_digit).count());
    let closest = format!("{v:.prec$e}", prec = digit_count.saturating_sub(1));
    let sci = if closest.parse::<f64>() == Ok(v) {
        closest
    } else {
        shortest
    };

    let (mantissa, exp) = sci.split_once('e').unwrap_or((&sci, "0"));
    let exp: i32 = exp.parse().unwrap_or(0);
    let (sign, mantissa) = mantissa
        .strip_prefix('-')
        .map_or(("", mantissa), |m| ("-", m));
    let digits: String = mantissa.chars().filter(char::is_ascii_digit).collect();

    let mut out = String::with_capacity(digits.len() + 8);
    out.push_str(sign);
    if !(-4..6).contains(&exp) {
        let (first, rest) = digits.split_at(1);
        out.push_str(first);
        if !rest.is_empty() {
            out.push('.');
            out.push_str(rest);
        }
        out.push_str(if exp < 0 { "e-" } else { "e+" });
        let abs = exp.unsigned_abs();
        if abs < 10 {
            out.push('0');
        }
        out.push_str(&abs.to_string());
        return out;
    }

    // -4 <= exp < 6, so the decimal point lands within a few places of the digits.
    let point = exp + 1;
    match usize::try_from(point) {
        Ok(0) | Err(_) => {
            out.push_str("0.");
            for _ in point..0 {
                out.push('0');
            }
            out.push_str(&digits);
        }
        Ok(point) if point >= digits.len() => {
            out.push_str(&digits);
            for _ in digits.len()..point {
                out.push('0');
            }
        }
        Ok(point) => {
            let (int, frac) = digits.split_at(point);
            out.push_str(int);
            out.push('.');
            out.push_str(frac);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Provider;
    use libdd_trace_model::StringData;
    use std::collections::HashMap;

    #[test]
    fn formats_floats_like_go() {
        // Expected values are the output of Go's fmt.Sprintf("%g", v).
        let cases: &[(f64, &str)] = &[
            (1.0, "1"),
            (0.5, "0.5"),
            (-2.5, "-2.5"),
            (100.0, "100"),
            (123_456.0, "123456"),
            (1_234_567.0, "1.234567e+06"),
            (1e6, "1e+06"),
            (1e21, "1e+21"),
            (1e100, "1e+100"),
            (0.0001, "0.0001"),
            (0.000_123, "0.000123"),
            (0.00001, "1e-05"),
            (1.234_567e-7, "1.234567e-07"),
            (12.345, "12.345"),
            (0.1 + 0.2, "0.30000000000000004"),
            (f64::MAX, "1.7976931348623157e+308"),
            (5e-324, "5e-324"),
            // 1 + 2^-17 = 1.00000762939453125 is exactly halfway between two 17-digit values: Go
            // rounds to even, Rust's shortest output rounds up.
            (f64::from_bits(0x3FF0_0008_0000_0000), "1.0000076293945312"),
            (
                -f64::from_bits(0x3FF0_0008_0000_0000),
                "-1.0000076293945312",
            ),
            (0.0, "0"),
            (-0.0, "-0"),
            (f64::NAN, "NaN"),
            (f64::INFINITY, "+Inf"),
            (f64::NEG_INFINITY, "-Inf"),
        ];
        for (v, want) in cases {
            assert_eq!(format_go_g(*v), *want, "formatting {v:?}");
        }
    }

    #[test]
    fn parses_floats_like_go() {
        // Expected values are the output of Go's strconv.ParseFloat(s, 64); None is an error.
        let cases: &[(&str, Option<f64>)] = &[
            ("1", Some(1.0)),
            ("+5", Some(5.0)),
            ("-1.5", Some(-1.5)),
            ("1.", Some(1.0)),
            (".5", Some(0.5)),
            ("1E3", Some(1000.0)),
            ("1e+3", Some(1000.0)),
            ("2_00", Some(200.0)),
            ("1_000.5", Some(1000.5)),
            ("1e1_0", Some(1e10)),
            ("1e-400", Some(0.0)),
            ("0x1p3", Some(8.0)),
            ("0X1.8P1", Some(3.0)),
            ("0x_1p0", Some(1.0)),
            ("-0x.8p0", Some(-0.5)),
            ("0x1p-1074", Some(5e-324)),
            ("0x1.fffffffffffff8p1023", None),
            ("0x1fffffffffffffffp0", Some(2_305_843_009_213_693_952.0)),
            ("0x1p-1075", Some(0.0)),
            ("0x1.00000000000008p0", Some(1.0)),
            ("0x1.000000000000081p0", Some(1.000_000_000_000_000_2)),
            ("0x1_0p0", Some(16.0)),
            ("0x1p1_0", Some(1024.0)),
            ("0x1.p0", Some(1.0)),
            ("0x.p1", None),
            ("1e1000000000000000000000", None),
            ("0x1p-99999999999999999999", Some(0.0)),
            ("0x1.000000000000001p-1075", Some(f64::from_bits(1))),
            ("0x1.00000000000001p-1075", Some(f64::from_bits(1))),
            ("0x1p-1076", Some(0.0)),
            ("0x3p-1075", Some(f64::from_bits(2))),
            ("0x1.8p-1074", Some(f64::from_bits(2))),
            (
                "0x1.fffffffffffffp-1023",
                Some(f64::from_bits(0x10_0000_0000_0000)),
            ),
            (
                "0x1.fffffffffffff8p-1023",
                Some(f64::from_bits(0x10_0000_0000_0000)),
            ),
            ("0x0.0000000000001p-1022", Some(f64::from_bits(1))),
            (
                "0x1.00000000000008p-1022",
                Some(f64::from_bits(0x10_0000_0000_0000)),
            ),
            (
                "0x1.000000000000081p-1022",
                Some(f64::from_bits(0x10_0000_0000_0001)),
            ),
            (
                "0x123456789abcdef0123p-1100",
                Some(f64::from_bits(0x48d1_59e2_6af3)),
            ),
            ("--1", None),
            ("+-1", None),
            ("-+1", None),
            ("++1", None),
            ("inf", Some(f64::INFINITY)),
            ("+Inf", Some(f64::INFINITY)),
            ("-infinity", Some(f64::NEG_INFINITY)),
            ("1e400", None),
            ("-1e400", None),
            ("0x1p1024", None),
            ("+nan", None),
            ("-nan", None),
            ("", None),
            ("-", None),
            (".", None),
            ("  1", None),
            ("1e", None),
            ("0x1", None),
            ("0x1p", None),
            ("0xp1", None),
            ("1__0", None),
            ("_1", None),
            ("1_", None),
            ("1_.5", None),
            ("0b101", None),
            ("0o7", None),
            ("abc", None),
        ];
        for (s, want) in cases {
            assert_eq!(parse_go_float(s), *want, "parsing {s:?}");
        }
        assert!(parse_go_float("nan").is_some_and(f64::is_nan));
        assert!(parse_go_float("NaN").is_some_and(f64::is_nan));
    }

    struct Attrs(HashMap<&'static str, &'static str>);

    impl Attributes for Attrs {
        type Text = String;
        type Bytes = Vec<u8>;
        type Values = StringData;

        fn attribute_str(&self, key: &str) -> Option<&str> {
            self.0.get(key).copied()
        }

        fn attribute_f64(&self, _: &str) -> Option<f64> {
            None
        }
    }

    const fn entry(name: &'static str, when: &'static [Condition]) -> Entry {
        Entry {
            name,
            provider: Provider::Otel,
            version: None,
            value_type: ValueType::String,
            when,
        }
    }

    const fn cond(
        attribute: &'static str,
        present: Option<bool>,
        eq: Option<&'static str>,
    ) -> Condition {
        Condition {
            attribute,
            present,
            eq,
        }
    }

    #[test]
    fn conditions() {
        // The embedded registry has no `present: true` condition, so test them on made-up entries.
        static ENTRIES: [Entry; 3] = [
            entry("a", &[cond("x", Some(true), None)]),
            entry(
                "b",
                &[cond("x", Some(false), None), cond("y", None, Some("on"))],
            ),
            entry("d", &[]),
        ];
        static PRESENT_AND_EQ: [Entry; 2] = [
            entry("c", &[cond("x", Some(true), Some("on"))]),
            entry("d", &[]),
        ];
        let attrs = |pairs: &[(&'static str, &'static str)]| {
            Attrs(
                [("a", "A"), ("b", "B"), ("c", "C"), ("d", "D")]
                    .into_iter()
                    .chain(pairs.iter().copied())
                    .collect(),
            )
        };
        let cases: &[(&[(&str, &str)], &str)] = &[
            (&[("x", "on")], "A"),
            (&[("x", "")], "D"),
            (&[("y", "on")], "B"),
            (&[("y", "off")], "D"),
            (&[], "D"),
        ];
        for (pairs, want) in cases {
            assert_eq!(
                str_in(&attrs(pairs), &ENTRIES).as_deref(),
                Some(*want),
                "{pairs:?}"
            );
        }

        assert_eq!(
            str_in(&attrs(&[("x", "on")]), &PRESENT_AND_EQ).as_deref(),
            Some("C")
        );
        assert_eq!(
            str_in(&attrs(&[("x", "off")]), &PRESENT_AND_EQ).as_deref(),
            Some("D")
        );
    }
}
