// Copyright 2025-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! A simple wrapper around `chrono`, so users don’t have to depend on it directly.
use chrono::{DateTime, Datelike, NaiveDate, TimeDelta, Utc};
use serde::{Deserialize, Serialize};

pub type Timestamp = DateTime<Utc>;

/// Timestamp policy shared by subject cutoffs and allocation activation.
#[derive(Debug, Clone, Serialize)]
#[serde(transparent)]
pub(crate) struct OptOutTimestamp(pub Timestamp);

impl OptOutTimestamp {
    pub fn parse(value: &str) -> Result<Self, &'static str> {
        parse_opt_out_timestamp(value)
            .map(Self)
            .ok_or("invalid opt-out timestamp")
    }
}

// D005/D006/D007: exact ASCII fields, lowercase t/z, fractional truncation, and
// UTC years 0000..9999. Chrono supplies calendar arithmetic, not the input grammar.
fn parse_opt_out_timestamp(value: &str) -> Option<Timestamp> {
    let bytes = value.as_bytes();
    if bytes.len() < 20
        || bytes[4] != b'-'
        || bytes[7] != b'-'
        || !matches!(bytes[10], b'T' | b't')
        || bytes[13] != b':'
        || bytes[16] != b':'
    {
        return None;
    }

    // All calls use fixed-width fields of at most four digits.
    let decimal = |digits: &[u8]| {
        digits.iter().try_fold(0_u32, |value, digit| {
            digit
                .is_ascii_digit()
                .then(|| value * 10 + u32::from(digit - b'0'))
        })
    };
    let year = i32::try_from(decimal(&bytes[..4])?).ok()?;
    let date = NaiveDate::from_ymd_opt(year, decimal(&bytes[5..7])?, decimal(&bytes[8..10])?)?;
    let hour = decimal(&bytes[11..13])?;
    let minute = decimal(&bytes[14..16])?;
    let second = decimal(&bytes[17..19])?;

    let mut position = 19;
    let mut nanos = 0;
    if bytes[position] == b'.' {
        position += 1;
        let start = position;
        let mut scale = 100_000_000;
        while let Some(digit) = bytes.get(position).filter(|digit| digit.is_ascii_digit()) {
            // Inspect every digit, but discard precision beyond nanoseconds without rounding.
            nanos += u32::from(digit - b'0') * scale;
            scale /= 10;
            position += 1;
        }
        if position == start {
            return None;
        }
    }
    let zone = &bytes[position..];
    let offset = match zone {
        [b'Z' | b'z'] => 0,
        [b'+' | b'-', _, _, b':', _, _] => {
            let hours = decimal(&zone[1..3])?;
            let minutes = decimal(&zone[4..6])?;
            if hours > 23 || minutes > 59 {
                return None;
            }
            let seconds = i64::from(hours * 3600 + minutes * 60);
            if zone[0] == b'-' { -seconds } else { seconds }
        }
        _ => return None,
    };
    let timestamp = date
        .and_hms_nano_opt(hour, minute, second, nanos)?
        .and_utc()
        .checked_sub_signed(TimeDelta::seconds(offset))?;
    (0..=9999).contains(&timestamp.year()).then_some(timestamp)
}

impl<'de> Deserialize<'de> for OptOutTimestamp {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let value = String::deserialize(deserializer)?;
        Self::parse(&value).map_err(serde::de::Error::custom)
    }
}

pub fn now() -> Timestamp {
    Utc::now()
}
