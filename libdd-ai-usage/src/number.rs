// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! Exact number handling: counts, the 2^53-1 bound, and USD to nano-USD.
//!
//! Nothing here casts with `as`. A double becomes an integer only through
//! [`exact_u64`], which reads the bits of a value already known to be an
//! integer in range, and an integer becomes a double through
//! [`integer_to_f64`], which is exact up to 2^53.

/// 2^53-1: the largest count or nano-USD amount every JSON reader holds
/// exactly.
pub const MAX_SAFE_INTEGER: u64 = 9_007_199_254_740_991;

const TWO_POW_32: f64 = 4_294_967_296.0;
const NANO_PER_USD: u64 = 1_000_000_000;
/// 10^7 USD is above 2^53-1 nano-USD, so nothing at or above it converts.
const USD_CONVERSION_LIMIT: f64 = 1e7;
const NANO_DIGITS: usize = 9;

/// How a JSON number reads as a count.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CountReading {
    /// A non-negative integer of at most 2^53-1.
    Count(u64),
    /// A non-negative integer above 2^53-1, or a number no double holds.
    AboveBound,
    /// Negative, fractional, or not a number.
    Malformed,
}

/// Classify a JSON number as a count. Positive infinity stands for a token
/// too large for a double and is tested before anything else, so it is never
/// called malformed.
pub fn read_count(value: f64) -> CountReading {
    if value == f64::INFINITY {
        return CountReading::AboveBound;
    }
    if !value.is_finite() || value < 0.0 || value.fract() != 0.0 {
        return CountReading::Malformed;
    }
    match exact_u64(value) {
        Some(count) => CountReading::Count(count),
        None => CountReading::AboveBound,
    }
}

/// The integer a double states, when it is a whole number in `[0, 2^53-1]`.
pub fn exact_u64(value: f64) -> Option<u64> {
    exact_integer(value)
        .and_then(|integer| u64::try_from(integer).ok())
        .filter(|integer| *integer <= MAX_SAFE_INTEGER)
}

/// The integer a double states, when it is a non-negative whole number below
/// 2^128. Every such double is an integer exactly, so nothing is rounded.
///
/// The value is read from the bits of the double: it is the 53-bit
/// significand times `2^(exponent - 1075)`. Below 2^53 the significand is
/// shifted right, and the shifted-out bits are zero because the value has no
/// fraction; from 2^53 up it is shifted left.
pub fn exact_integer(value: f64) -> Option<u128> {
    if !value.is_finite() || value < 0.0 || value.fract() != 0.0 {
        return None;
    }
    if value == 0.0 {
        return Some(0);
    }
    let bits = value.to_bits();
    let exponent = (bits >> 52) & 0x7ff;
    let significand = u128::from((bits & ((1u64 << 52) - 1)) | (1u64 << 52));
    match exponent.checked_sub(1075) {
        Some(left) => {
            let left = u32::try_from(left).ok().filter(|shift| *shift <= 75)?;
            Some(significand << left)
        }
        None => {
            let right = u32::try_from(1075 - exponent)
                .ok()
                .filter(|shift| *shift <= 52)?;
            Some(significand >> right)
        }
    }
}

/// The double nearest to an integer: exact up to 2^53, and above it the
/// nearest double, with a tie going to the one whose last bit is zero.
pub fn integer_to_f64(value: u128) -> f64 {
    let exact = u64::try_from(value)
        .ok()
        .filter(|v| *v <= MAX_SAFE_INTEGER + 1)
        .and_then(|v| {
            Some((
                u32::try_from(v >> 32).ok()?,
                u32::try_from(v & 0xffff_ffff).ok()?,
            ))
        });
    match exact {
        // Both halves and the product are exact, and the sum is at most 2^53.
        Some((high, low)) => f64::from(high) * TWO_POW_32 + f64::from(low),
        // Decimal parsing is correctly rounded, which a cast would also be;
        // this path keeps the module free of numeric casts.
        None => value.to_string().parse::<f64>().unwrap_or(f64::INFINITY),
    }
}

/// The nano-USD value of a USD amount, or `None` when the amount is unusable
/// (`PORTABLE-METRICS.md#nano-usd-of-a-usd-number`).
///
/// The order is the contract's: reject a negative or non-finite amount, then
/// reject `usd >= 1e7` on the number itself, and only then convert. The
/// conversion takes the shortest decimal that reads back as the same double
/// and rounds it half up to nine decimal places on the digits.
///
/// The shortest decimal comes from `f64`'s `Display`, which the standard
/// library documents as the shortest representation that round-trips, and
/// which never prints an exponent.
pub fn nano_usd_from_usd(usd: f64) -> Option<u64> {
    // The range test is false for NaN and for both infinities.
    if !(0.0..USD_CONVERSION_LIMIT).contains(&usd) {
        return None;
    }
    let text = usd.to_string();
    // Negative zero prints a sign and is zero.
    let text = text.strip_prefix('-').unwrap_or(&text);
    let (whole, fraction) = text.split_once('.').unwrap_or((text, ""));
    let whole: u64 = whole.parse().ok()?;
    if !fraction.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let (kept, dropped) = fraction.split_at(fraction.len().min(NANO_DIGITS));
    let mut nanos: u64 = 0;
    for index in 0..NANO_DIGITS {
        let digit = kept
            .as_bytes()
            .get(index)
            .map_or(0, |b| u64::from(b - b'0'));
        nanos = nanos.checked_mul(10)?.checked_add(digit)?;
    }
    // Half up on the digits: the first dropped digit decides.
    let round_up = dropped.as_bytes().first().is_some_and(|b| *b >= b'5');
    let total = whole
        .checked_mul(NANO_PER_USD)?
        .checked_add(nanos)?
        .checked_add(u64::from(round_up))?;
    (total <= MAX_SAFE_INTEGER).then_some(total)
}

/// The nano-USD amount a `cost_nano_usd` number states, or `None` when it is
/// unusable: negative, fractional, above 2^53-1, or not finite.
pub fn nano_usd_from_number(value: f64) -> Option<u64> {
    match read_count(value) {
        CountReading::Count(nanos) => Some(nanos),
        CountReading::AboveBound | CountReading::Malformed => None,
    }
}

/// USD from nano-USD: one IEEE 754 double division.
pub fn usd_from_nano_usd(nanos: u64) -> f64 {
    integer_to_f64(u128::from(nanos)) / 1e9
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn usd_converts_on_the_shortest_decimal_never_in_floating_point() {
        // The floating product of this amount is below the half and rounds
        // down, whatever the tie rule.
        let usd = 1.0050000005_f64;
        assert_eq!(nano_usd_from_usd(usd), Some(1_005_000_001));
        assert!(usd * 1e9 < 1_005_000_000.5);
        assert_eq!(exact_u64((usd * 1e9).round()), Some(1_005_000_000));

        // The floating product of this amount is exactly a tie. `round`
        // takes ties away from zero and happens to agree; ties-to-even does
        // not. The digits decide, not the tie rule of a float rounding.
        let usd = 0.0010000005_f64;
        assert_eq!(nano_usd_from_usd(usd), Some(1_000_001));
        assert_eq!(usd * 1e9, 1_000_000.5);
        assert_eq!(exact_u64((usd * 1e9).round_ties_even()), Some(1_000_000));
    }

    #[test]
    fn usd_conversion_matches_the_contract_examples() {
        for (usd, nanos) in [
            (0.0105, 10_500_000),
            (0.30000000000000004, 300_000_000),
            (0.0000000005, 1),
            (0.0000000004, 0),
            (0.25, 250_000_000),
            (2.1e-05, 21_000),
            (2.1e-06, 2_100),
            (0.0, 0),
            (-0.0, 0),
            (1.0, 1_000_000_000),
            (9_007_199.254_740_99, 9_007_199_254_740_990),
            (5e-324, 0),
            (0.999_999_999_5, 1_000_000_000),
            (0.999_999_999_4, 999_999_999),
        ] {
            assert_eq!(nano_usd_from_usd(usd), Some(nanos), "{usd}");
        }
    }

    #[test]
    fn usd_at_or_above_the_limit_is_unusable_before_any_conversion() {
        for usd in [
            1e7,
            9_007_199.26,
            1e21,
            1e300,
            f64::MAX,
            f64::INFINITY,
            f64::NEG_INFINITY,
            f64::NAN,
            -0.5,
            -1e-300,
        ] {
            assert_eq!(nano_usd_from_usd(usd), None, "{usd}");
        }
        // The largest double below 10^7 passes the first test, converts,
        // and is then above the bound.
        let below_limit = 1e7_f64.next_down();
        assert!(below_limit < 1e7);
        assert_eq!(nano_usd_from_usd(below_limit), None);
    }

    #[test]
    fn the_standard_library_prints_the_shortest_decimal_without_an_exponent() {
        for (value, text) in [
            (5e-10_f64, "0.0000000005"),
            (2.1e-5, "0.000021"),
            (0.1 + 0.2, "0.30000000000000004"),
            (1.0050000005, "1.0050000005"),
            (1e21, "1000000000000000000000"),
            (100.0, "100"),
        ] {
            assert_eq!(value.to_string(), text);
            assert_eq!(text.parse::<f64>().unwrap().to_bits(), value.to_bits());
        }
    }

    #[test]
    fn counts_read_exactly_at_and_around_the_bound() {
        assert_eq!(read_count(0.0), CountReading::Count(0));
        assert_eq!(read_count(1000.0), CountReading::Count(1000));
        assert_eq!(
            read_count(9_007_199_254_740_991.0),
            CountReading::Count(MAX_SAFE_INTEGER)
        );
        assert_eq!(
            read_count(9_007_199_254_740_992.0),
            CountReading::AboveBound
        );
        assert_eq!(read_count(1e300), CountReading::AboveBound);
        assert_eq!(read_count(f64::INFINITY), CountReading::AboveBound);
        for malformed in [-1.0, 1.5, -0.5, f64::NEG_INFINITY, f64::NAN, 5e-324] {
            assert_eq!(
                read_count(malformed),
                CountReading::Malformed,
                "{malformed}"
            );
        }
    }

    #[test]
    fn exact_integer_conversion_round_trips_over_the_whole_range() {
        let mut samples = vec![0u64, 1, 2, 3, 255, 256, 1 << 31, (1 << 32) - 1, 1 << 32];
        for power in 0..53 {
            let base = 1u64 << power;
            samples.extend([base - 1, base, base + 1]);
        }
        samples.extend([MAX_SAFE_INTEGER - 1, MAX_SAFE_INTEGER]);
        for n in samples {
            let double = integer_to_f64(u128::from(n));
            assert_eq!(double.to_string(), n.to_string());
            assert_eq!(exact_u64(double), Some(n), "{n}");
        }
        assert_eq!(exact_u64(9_007_199_254_740_992.0), None);
        assert_eq!(exact_u64(0.5), None);
        assert_eq!(exact_u64(-1.0), None);
        assert_eq!(exact_u64(f64::NAN), None);
        // Above 2^53 the result is the nearest double, ties to even.
        assert_eq!(
            integer_to_f64(9_007_199_254_740_993),
            9_007_199_254_740_992.0
        );
        assert_eq!(
            integer_to_f64(9_007_199_254_740_995),
            9_007_199_254_740_996.0
        );
        assert_eq!(
            integer_to_f64(9_007_199_254_740_994),
            9_007_199_254_740_994.0
        );
        assert_eq!(
            integer_to_f64(u128::from(u64::MAX)),
            18_446_744_073_709_551_616.0
        );
    }

    #[test]
    fn nano_usd_numbers_and_the_histogram_division() {
        assert_eq!(nano_usd_from_number(250_000_000.0), Some(250_000_000));
        assert_eq!(
            nano_usd_from_number(9_007_199_254_740_991.0),
            Some(MAX_SAFE_INTEGER)
        );
        for unusable in [9_007_199_254_740_992.0, 1.5, -1.0, f64::INFINITY] {
            assert_eq!(nano_usd_from_number(unusable), None);
        }
        assert_eq!(usd_from_nano_usd(MAX_SAFE_INTEGER), 9_007_199.254_740_99);
        assert_eq!(usd_from_nano_usd(10_500_000), 0.0105);
    }

    #[test]
    fn every_whole_double_below_two_to_the_128_is_an_exact_integer() {
        for (double, integer) in [
            (0.0_f64, 0_u128),
            (1.0, 1),
            (9_007_199_254_740_992.0, 1 << 53),
            (9_007_199_254_740_994.0, (1 << 53) + 2),
            (18_446_744_073_709_551_616.0, 1 << 64),
            (1e20, 100_000_000_000_000_000_000),
            (2.0_f64.powi(127), 1 << 127),
        ] {
            assert_eq!(exact_integer(double), Some(integer), "{double}");
            assert_eq!(integer_to_f64(integer), double, "{integer}");
        }
        for not_integer in [0.5, -1.0, 2.0_f64.powi(128), f64::INFINITY, f64::NAN, 1e300] {
            assert_eq!(exact_integer(not_integer), None, "{not_integer}");
        }
        assert_eq!(exact_integer(-0.0), Some(0));
    }
}
