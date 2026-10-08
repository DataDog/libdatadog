// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! The Datadog binding of the metric profiles: the series an exporter submits
//! for a profile's points, and their DogStatsD encoding.
//!
//! - A metric keeps its name. A Counter becomes a Count; a Histogram becomes one Distribution
//!   sample per input sample, plus `{name}.sum` and `{name}.count` Counts.
//! - A Counter in `{nanoUSD}` is summed exactly and submitted in USD, divided by 10^9 once.
//! - Every series carries `trajectory.profile:{id}/{version}`.
//! - A tag key is the attribute name, except `service.name` and `host.name`, which become `service`
//!   and `host`. Tag values are normalized as Datadog would, so that what is sent is what is
//!   stored.

use std::collections::BTreeMap;
use std::fmt::Write as _;

use crate::Profile;
use crate::point::{Attributes, ErrorCode, Instrument, MetricError, MetricPoint, Sum, reject};
use crate::registry::PROFILE_ATTRIBUTE;

const NANO_USD: &str = "{nanoUSD}";
const NANO_USD_PER_USD: f64 = 1_000_000_000.0;

/// The type of a Datadog series.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum SeriesType {
    Count,
    Distribution,
}

impl SeriesType {
    pub fn as_str(self) -> &'static str {
        match self {
            SeriesType::Count => "count",
            SeriesType::Distribution => "distribution",
        }
    }

    /// The DogStatsD metric type.
    fn dogstatsd_type(self) -> &'static str {
        match self {
            SeriesType::Count => "c",
            SeriesType::Distribution => "d",
        }
    }
}

/// One series point to submit: a Count's value over the submission, or one
/// Distribution sample. Tags are sorted `key:value` strings.
#[derive(Debug, Clone, PartialEq)]
pub struct DatadogSeries {
    pub name: String,
    pub series_type: SeriesType,
    pub value: f64,
    pub tags: Vec<String>,
}

impl DatadogSeries {
    /// The DogStatsD line for this series point, without a trailing newline:
    /// `name:value|c|#tag,tag` or `name:value|d|#tag,tag`.
    pub fn to_dogstatsd(&self) -> String {
        let mut line = String::with_capacity(
            self.name.len() + 24 + self.tags.iter().map(|t| t.len() + 1).sum::<usize>(),
        );
        // Writing to a String cannot fail.
        let _ = write!(
            line,
            "{}:{}|{}",
            self.name,
            self.value,
            self.series_type.dogstatsd_type()
        );
        if !self.tags.is_empty() {
            line.push_str("|#");
            line.push_str(&self.tags.join(","));
        }
        line
    }
}

/// The Datadog tag key of an attribute.
fn tag_key(attribute: &str) -> &str {
    match attribute {
        "service.name" => "service",
        "host.name" => "host",
        other => other,
    }
}

fn is_renamed(attribute: &str) -> bool {
    tag_key(attribute) != attribute
}

/// Normalize a tag value as Datadog stores it: ASCII upper case becomes lower
/// case, and every code point outside `a-z`, `0-9`, `_`, `-`, `:`, `.` and `/`
/// becomes one `_`. Nothing is truncated, collapsed or trimmed.
pub fn datadog_tag_value(value: &str) -> String {
    value
        .chars()
        .map(|c| {
            let c = c.to_ascii_lowercase();
            if c.is_ascii_lowercase()
                || c.is_ascii_digit()
                || matches!(c, '_' | '-' | ':' | '.' | '/')
            {
                c
            } else {
                '_'
            }
        })
        .collect()
}

/// The tag value of an attribute: `{id}@{version}` values keep their version
/// apart with `/`, which Datadog keeps, instead of `@`, which it does not.
fn tag_value(attribute: &str, value: &str) -> String {
    if attribute == PROFILE_ATTRIBUTE || attribute == "trajectory.cost.rate_card" {
        datadog_tag_value(&value.replace('@', "/"))
    } else {
        datadog_tag_value(value)
    }
}

/// The tags of one carrier, keyed by tag key. When two attributes give one
/// key, the attribute this binding renames wins, whatever the order.
fn carrier_tags(attributes: &Attributes) -> BTreeMap<&str, (&str, &str)> {
    let mut tags: BTreeMap<&str, (&str, &str)> = BTreeMap::new();
    for (attribute, value) in attributes {
        let key = tag_key(attribute);
        match tags.get(key) {
            Some((existing, _)) if is_renamed(existing) => {}
            _ => {
                tags.insert(key, (attribute.as_str(), value.as_str()));
            }
        }
    }
    tags
}

/// The sorted tags of a point under `profile`: the profile tag, the point's
/// attributes, and the resource attributes the point does not override.
fn series_tags(
    profile: Profile,
    point: &MetricPoint,
    resource: &Attributes,
) -> Result<Vec<String>, MetricError> {
    if let Some(stated) = point.attributes.get(PROFILE_ATTRIBUTE) {
        if stated != profile.id() {
            return reject(
                ErrorCode::MetricProfileMismatch,
                format!(
                    "{} carries the profile {stated}, not {}",
                    point.name,
                    profile.id()
                ),
            );
        }
    }
    let mut tags = carrier_tags(resource);
    // A resource attribute never states the profile.
    tags.retain(|key, _| *key != PROFILE_ATTRIBUTE);
    // A point attribute wins over a resource attribute of the same key.
    tags.extend(carrier_tags(&point.attributes));
    let profile_id = profile.id();
    tags.insert(PROFILE_ATTRIBUTE, (PROFILE_ATTRIBUTE, profile_id));
    Ok(tags
        .into_iter()
        .map(|(key, (attribute, value))| format!("{key}:{}", tag_value(attribute, value)))
        .collect())
}

/// The series to submit for the points of one profile and one submission.
///
/// Every point must be a metric of `profile`; a point of another profile is
/// `metric_profile_mismatch`. Counts are summed per name and tag set; each
/// Histogram sample is one Distribution point, and its `.sum` and `.count`
/// Counts are summed in the order the samples were recorded. A `historical`
/// submission, for a past time Datadog does not accept Distributions for,
/// sends only the `.sum` and `.count` Counts.
///
/// The series are returned sorted by name, then tags, then value.
pub fn datadog_series(
    profile: Profile,
    points: &[MetricPoint],
    resource: &Attributes,
    historical: bool,
) -> Result<Vec<DatadogSeries>, MetricError> {
    let mut counts: BTreeMap<(String, Vec<String>), (Sum, bool)> = BTreeMap::new();
    let mut series = Vec::new();
    let mut count = |name: String, tags: Vec<String>, increment: Sum, scaled| {
        let entry = counts
            .entry((name, tags))
            .or_insert((Sum::Integer(0), scaled));
        entry.0 = entry.0.add(increment);
    };
    for point in points {
        if !profile.metric_names().contains(&point.name.as_str()) {
            return reject(
                ErrorCode::MetricProfileMismatch,
                format!("{} is not a metric of {}", point.name, profile.id()),
            );
        }
        let tags = series_tags(profile, point, resource)?;
        match point.instrument {
            Instrument::Counter => {
                count(
                    point.name.clone(),
                    tags,
                    Sum::of(point),
                    point.unit == NANO_USD,
                );
            }
            Instrument::Histogram => {
                if !historical {
                    series.push(DatadogSeries {
                        name: point.name.clone(),
                        series_type: SeriesType::Distribution,
                        value: point.value,
                        tags: tags.clone(),
                    });
                }
                // The sum adds doubles in recording order, like any Count of
                // a fractional value.
                count(
                    format!("{}.sum", point.name),
                    tags.clone(),
                    Sum::Double(point.value),
                    false,
                );
                count(
                    format!("{}.count", point.name),
                    tags,
                    Sum::Integer(1),
                    false,
                );
            }
        }
    }
    for ((name, tags), (total, scaled)) in counts {
        let value = if scaled {
            // One division of the exact sum, converted to a double once.
            total.value() / NANO_USD_PER_USD
        } else {
            total.value()
        };
        series.push(DatadogSeries {
            name,
            series_type: SeriesType::Count,
            value,
            tags,
        });
    }
    series.sort_by(|a, b| {
        a.name
            .cmp(&b.name)
            .then_with(|| a.tags.cmp(&b.tags))
            .then_with(|| a.value.total_cmp(&b.value))
    });
    Ok(series)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tag_values_are_normalized_one_character_per_code_point() {
        assert_eq!(datadog_tag_value("Ada@Example.com"), "ada_example.com");
        assert_eq!(datadog_tag_value("A  B"), "a__b");
        assert_eq!(datadog_tag_value("é"), "_");
        assert_eq!(datadog_tag_value("e\u{301}"), "e_");
        assert_eq!(datadog_tag_value(""), "");
        assert_eq!(datadog_tag_value("a-b_c:d.e/f"), "a-b_c:d.e/f");
    }

    #[test]
    fn dogstatsd_lines() {
        let series = DatadogSeries {
            name: "trajectory.gen_ai.client.inference.usage.cost".into(),
            series_type: SeriesType::Count,
            value: 0.3,
            tags: vec!["a:b".into(), "c:d".into()],
        };
        assert_eq!(
            series.to_dogstatsd(),
            "trajectory.gen_ai.client.inference.usage.cost:0.3|c|#a:b,c:d"
        );
        let series = DatadogSeries {
            name: "gen_ai.client.inference.duration".into(),
            series_type: SeriesType::Distribution,
            value: 12.0,
            tags: vec![],
        };
        assert_eq!(
            series.to_dogstatsd(),
            "gen_ai.client.inference.duration:12|d"
        );
        let tiny = DatadogSeries {
            value: 1e-10,
            ..series
        };
        assert_eq!(
            tiny.to_dogstatsd(),
            "gen_ai.client.inference.duration:0.0000000001|d"
        );
    }
}
