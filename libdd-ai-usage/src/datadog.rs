// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! The Datadog binding of the metric profiles: the series an exporter submits
//! to the Datadog API for a profile's points, and their DogStatsD encoding.
//!
//! - A metric keeps its name. A Counter becomes a Rate over [`DATADOG_RATE_INTERVAL_SECS`] (10 s):
//!   its total over the submission divided by 10, submitted with `interval: 10`. A Histogram
//!   becomes one Distribution sample per input sample, plus `{name}.sum` and `{name}.count` Rates.
//! - Over DogStatsD a Counter is a `c` of its total, which the Agent stores as the same Rate (see
//!   [`DATADOG_RATE_INTERVAL_SECS`]), and a Histogram sample is a `d`. Whichever transport carries
//!   it, a metric is stored with one type and one interval.
//! - A Counter is summed exactly, then divided once: by 10 for its Rate, and, in `{nanoUSD}`, by
//!   10^9 as well, to be submitted in USD.
//! - Every series carries `trajectory.profile:{id}/{version}`.
//! - A tag key is the attribute name, except `service.name` and `host.name`, which become `service`
//!   and `host`. Tag values are normalized as Datadog would, so that what is sent is what is
//!   stored.

use std::collections::BTreeMap;
use std::fmt::Write as _;

use crate::PROFILE_ATTRIBUTE;
use crate::Profile;
use crate::point::{Attributes, ErrorCode, Instrument, MetricError, MetricPoint, Sum, reject};

pub(crate) const NANO_USD: &str = "{nanoUSD}";
pub(crate) const NANO_USD_PER_USD: f64 = 1_000_000_000.0;

/// The interval, in seconds, of every Rate series this binding submits to the
/// Datadog API.
///
/// It is the Datadog Agent's DogStatsD flush interval. The Agent stores every
/// DogStatsD count (`c`) as a RATE whose value is the count over its 10 s
/// bucket divided by 10, with an interval of 10, and offers no way to keep it a
/// COUNT. Datadog keeps one type and one interval per metric, whichever writer
/// set it last, and reads every point of the metric with them (`as_count()`
/// on a RATE multiplies each value by the interval). A Counter submitted to
/// the API as anything else would make its metric's type flip between writers
/// and its totals read up to 10 times too high or too low; submitted as a
/// Rate over this interval, it registers exactly what DogStatsD through the
/// Agent registers.
pub const DATADOG_RATE_INTERVAL_SECS: u32 = 10;

/// The type of a Datadog series.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum SeriesType {
    /// A Counter: submitted to the API as a RATE (API v2 type 2) over
    /// [`DATADOG_RATE_INTERVAL_SECS`], over DogStatsD as a `c`.
    Rate,
    /// A Histogram sample: a DISTRIBUTION, over DogStatsD a `d`.
    Distribution,
}

impl SeriesType {
    /// The type's name in the Datadog API: `rate` or `distribution`.
    pub fn as_str(self) -> &'static str {
        match self {
            SeriesType::Rate => "rate",
            SeriesType::Distribution => "distribution",
        }
    }

    /// The interval a series of this type is submitted with: 10 s for a
    /// Rate, none for a Distribution.
    pub fn interval(self) -> Option<u32> {
        match self {
            SeriesType::Rate => Some(DATADOG_RATE_INTERVAL_SECS),
            SeriesType::Distribution => None,
        }
    }

    /// The DogStatsD metric type.
    fn dogstatsd_type(self) -> &'static str {
        match self {
            SeriesType::Rate => "c",
            SeriesType::Distribution => "d",
        }
    }
}

/// One series point to submit to the Datadog API, exactly as it is to be
/// submitted: its type, interval and value are what the Agent stores for the
/// same points sent over DogStatsD, and a submitter must not change them.
/// Tags are sorted `key:value` strings.
#[derive(Debug, Clone, PartialEq)]
pub struct DatadogSeries {
    pub name: String,
    pub series_type: SeriesType,
    /// A Rate's value: the Counter's total over the submission divided by
    /// `interval`. A Distribution's: one sample.
    pub value: f64,
    /// `Some(DATADOG_RATE_INTERVAL_SECS)` for a Rate, `None` for a
    /// Distribution.
    pub interval: Option<u32>,
    pub tags: Vec<String>,
}

/// The DogStatsD line of one series point, without a trailing newline:
/// `name:value|c|#tag,tag` or `name:value|d|#tag,tag`. A Counter's value is
/// its total, not its Rate: the Agent divides it by its flush interval.
fn dogstatsd_line(name: &str, series_type: SeriesType, value: f64, tags: &[String]) -> String {
    let mut line =
        String::with_capacity(name.len() + 24 + tags.iter().map(|t| t.len() + 1).sum::<usize>());
    // Writing to a String cannot fail.
    let _ = write!(line, "{name}:{value}|{}", series_type.dogstatsd_type());
    if !tags.is_empty() {
        line.push_str("|#");
        line.push_str(&tags.join(","));
    }
    line
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

/// Whether an attribute's values are `{id}@{version}` identifiers.
pub(crate) fn is_versioned(attribute: &str) -> bool {
    attribute == PROFILE_ATTRIBUTE || attribute == "trajectory.cost.rate_card"
}

/// The tag value of an attribute: `{id}@{version}` values keep their version
/// apart with `/`, which Datadog keeps, instead of `@`, which it does not.
fn tag_value(attribute: &str, value: &str) -> String {
    if is_versioned(attribute) {
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
    tags_of(profile, &point.name, &point.attributes, resource)
}

fn tags_of(
    profile: Profile,
    name: &str,
    attributes: &Attributes,
    resource: &Attributes,
) -> Result<Vec<String>, MetricError> {
    if let Some(stated) = attributes.get(PROFILE_ATTRIBUTE)
        && stated != profile.id()
    {
        return reject(
            ErrorCode::MetricProfileMismatch,
            format!("{name} carries the profile {stated}, not {}", profile.id()),
        );
    }
    let mut tags = carrier_tags(resource);
    // A resource attribute never states the profile.
    tags.retain(|key, _| *key != PROFILE_ATTRIBUTE);
    // A point attribute wins over a resource attribute of the same key.
    tags.extend(carrier_tags(attributes));
    let profile_id = profile.id();
    tags.insert(PROFILE_ATTRIBUTE, (PROFILE_ATTRIBUTE, profile_id));
    Ok(tags
        .into_iter()
        .map(|(key, (attribute, value))| format!("{key}:{}", tag_value(attribute, value)))
        .collect())
}

/// The series to submit to the Datadog API for the points of one profile and
/// one submission.
///
/// Every point must be a metric of `profile`; a point of another profile is
/// `metric_profile_mismatch`. Counters are summed per name and tag set and
/// submitted as Rates over [`DATADOG_RATE_INTERVAL_SECS`]; each Histogram
/// sample is one Distribution point, and its `.sum` and `.count` Rates are
/// summed in the order the samples were recorded. A `historical` submission,
/// for a past time Datadog does not accept Distributions for, sends only the
/// `.sum` and `.count` Rates.
///
/// The series are returned sorted by name, then tags, then value.
pub fn datadog_series(
    profile: Profile,
    points: &[MetricPoint],
    resource: &Attributes,
    historical: bool,
) -> Result<Vec<DatadogSeries>, MetricError> {
    let mut builder = SeriesBuilder::default();
    builder.add(profile, points, resource, historical)?;
    Ok(builder.finish())
}

/// Series of one submission, built from the points of any number of profiles
/// and resources. Counters are summed by their final name and tag set, so
/// carriers whose tags normalize to the same series give one Rate, added
/// exactly before it is scaled.
#[derive(Debug, Default)]
pub(crate) struct SeriesBuilder {
    counts: BTreeMap<(String, Vec<String>), (Sum, bool)>,
    samples: Vec<Sample>,
    distributions: Vec<Distribution>,
    distribution_index: BTreeMap<(String, Vec<String>), usize>,
    /// Keep each distribution's samples instead of one series per sample.
    pub(crate) keep_samples: bool,
}

/// One Distribution sample, submitted as its own series point.
#[derive(Debug)]
struct Sample {
    name: String,
    tags: Vec<String>,
    value: f64,
}

/// One Distribution series: its samples' sum and count in recording order,
/// and the samples when kept.
#[derive(Debug)]
pub(crate) struct Distribution {
    pub(crate) name: String,
    pub(crate) tags: Vec<String>,
    sum: f64,
    count: u64,
    pub(crate) samples: Vec<f64>,
}

/// One series point of a submission, before it is encoded for a transport.
enum Value {
    /// A Counter's exact total over the submission, and whether it is in
    /// `{nanoUSD}`.
    Counter {
        total: Sum,
        nano_usd: bool,
    },
    Sample(f64),
}

impl Value {
    fn series_type(&self) -> SeriesType {
        match self {
            Value::Counter { .. } => SeriesType::Rate,
            Value::Sample(_) => SeriesType::Distribution,
        }
    }

    /// The value DogStatsD sends: a Counter's total (in USD for
    /// `{nanoUSD}`), or the sample.
    fn total(&self) -> f64 {
        match *self {
            // One division of the exact sum, converted to a double once.
            Value::Counter {
                total,
                nano_usd: true,
            } => total.value() / NANO_USD_PER_USD,
            Value::Counter { total, .. } => total.value(),
            Value::Sample(value) => value,
        }
    }

    /// The value the API takes: a Counter's total divided by the Rate
    /// interval (and by 10^9 for `{nanoUSD}`) in one division of the exact
    /// sum, or the sample.
    fn api_value(&self) -> f64 {
        let interval = f64::from(DATADOG_RATE_INTERVAL_SECS);
        match *self {
            Value::Counter {
                total,
                nano_usd: true,
            } => total.value() / (NANO_USD_PER_USD * interval),
            Value::Counter { total, .. } => total.value() / interval,
            Value::Sample(value) => value,
        }
    }
}

impl SeriesBuilder {
    /// The Distribution a Histogram series of `profile` is submitted as,
    /// tagged once for all its samples. Series whose tags normalize alike
    /// share one.
    pub(crate) fn distribution(
        &mut self,
        profile: Profile,
        series: &crate::batch::Series,
        resource: &Attributes,
    ) -> Result<usize, MetricError> {
        if !profile.metric_names().contains(&series.name.as_str()) {
            return reject(
                ErrorCode::MetricProfileMismatch,
                format!("{} is not a metric of {}", series.name, profile.id()),
            );
        }
        let tags = tags_of(profile, &series.name, &series.attributes, resource)?;
        let key = (series.name.clone(), tags);
        if let Some(at) = self.distribution_index.get(&key) {
            return Ok(*at);
        }
        let at = self.distributions.len();
        self.distributions.push(Distribution {
            name: key.0.clone(),
            tags: key.1.clone(),
            sum: 0.0,
            count: 0,
            samples: Vec::new(),
        });
        self.distribution_index.insert(key, at);
        Ok(at)
    }

    /// Record one sample of a Distribution, in recording order.
    pub(crate) fn sample(&mut self, distribution: usize, value: f64, historical: bool) {
        let Some(held) = self.distributions.get_mut(distribution) else {
            return;
        };
        held.sum += value;
        held.count += 1;
        if self.keep_samples {
            held.samples.push(value);
        } else if !historical {
            self.samples.push(Sample {
                name: held.name.clone(),
                tags: held.tags.clone(),
                value,
            });
        }
    }

    /// The Rates and the kept Distributions, the Rates sorted.
    pub(crate) fn finish_parts(mut self) -> (Vec<DatadogSeries>, Vec<Distribution>) {
        let distributions = self.close();
        (self.api_series(), distributions)
    }

    /// Add the `.sum` and `.count` Counters of the Distributions, and take
    /// the Distributions.
    fn close(&mut self) -> Vec<Distribution> {
        let distributions = std::mem::take(&mut self.distributions);
        for held in &distributions {
            self.count(
                format!("{}.sum", held.name),
                held.tags.clone(),
                Sum::Double(held.sum),
                false,
            );
            self.count(
                format!("{}.count", held.name),
                held.tags.clone(),
                Sum::Integer(u128::from(held.count)),
                false,
            );
        }
        distributions
    }

    pub(crate) fn add(
        &mut self,
        profile: Profile,
        points: &[MetricPoint],
        resource: &Attributes,
        historical: bool,
    ) -> Result<(), MetricError> {
        // Check every point first, so that a refused submission adds nothing.
        let mut tagged = Vec::with_capacity(points.len());
        for point in points {
            if !profile.metric_names().contains(&point.name.as_str()) {
                return reject(
                    ErrorCode::MetricProfileMismatch,
                    format!("{} is not a metric of {}", point.name, profile.id()),
                );
            }
            tagged.push((point, series_tags(profile, point, resource)?));
        }
        for (point, tags) in tagged {
            match point.instrument {
                Instrument::Counter => {
                    self.count(
                        point.name.clone(),
                        tags,
                        Sum::of(point),
                        point.unit == NANO_USD,
                    );
                }
                Instrument::Histogram => {
                    if !historical {
                        self.samples.push(Sample {
                            name: point.name.clone(),
                            tags: tags.clone(),
                            value: point.value,
                        });
                    }
                    // The sum adds doubles in recording order, like any
                    // Counter of a fractional value.
                    self.count(
                        format!("{}.sum", point.name),
                        tags.clone(),
                        Sum::Double(point.value),
                        false,
                    );
                    self.count(
                        format!("{}.count", point.name),
                        tags,
                        Sum::Integer(1),
                        false,
                    );
                }
            }
        }
        Ok(())
    }

    fn count(&mut self, name: String, tags: Vec<String>, increment: Sum, nano_usd: bool) {
        let entry = self
            .counts
            .entry((name, tags))
            .or_insert((Sum::Integer(0), nano_usd));
        entry.0 = entry.0.add(increment);
    }

    /// The series to submit to the API.
    pub(crate) fn finish(mut self) -> Vec<DatadogSeries> {
        self.close();
        self.api_series()
    }

    /// The DogStatsD lines of the submission, in the order of
    /// [`SeriesBuilder::finish`].
    pub(crate) fn finish_dogstatsd(mut self) -> Vec<String> {
        self.close();
        self.points()
            .into_iter()
            .map(|(name, tags, value)| {
                dogstatsd_line(&name, value.series_type(), value.total(), &tags)
            })
            .collect()
    }

    fn api_series(self) -> Vec<DatadogSeries> {
        self.points()
            .into_iter()
            .map(|(name, tags, value)| {
                let series_type = value.series_type();
                DatadogSeries {
                    name,
                    series_type,
                    value: value.api_value(),
                    interval: series_type.interval(),
                    tags,
                }
            })
            .collect()
    }

    /// Every point, sorted by name, then tags, then value.
    fn points(mut self) -> Vec<(String, Vec<String>, Value)> {
        let mut points: Vec<(String, Vec<String>, Value)> =
            Vec::with_capacity(self.counts.len() + self.samples.len());
        for ((name, tags), (total, nano_usd)) in std::mem::take(&mut self.counts) {
            points.push((name, tags, Value::Counter { total, nano_usd }));
        }
        for Sample { name, tags, value } in self.samples {
            points.push((name, tags, Value::Sample(value)));
        }
        points.sort_by(|a, b| {
            a.0.cmp(&b.0)
                .then_with(|| a.1.cmp(&b.1))
                .then_with(|| a.2.total().total_cmp(&b.2.total()))
        });
        points
    }
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
        assert_eq!(
            dogstatsd_line(
                "trajectory.gen_ai.client.inference.usage.cost",
                SeriesType::Rate,
                0.3,
                &["a:b".into(), "c:d".into()],
            ),
            "trajectory.gen_ai.client.inference.usage.cost:0.3|c|#a:b,c:d"
        );
        let name = "gen_ai.client.inference.duration";
        assert_eq!(
            dogstatsd_line(name, SeriesType::Distribution, 12.0, &[]),
            "gen_ai.client.inference.duration:12|d"
        );
        assert_eq!(
            dogstatsd_line(name, SeriesType::Distribution, 1e-10, &[]),
            "gen_ai.client.inference.duration:0.0000000001|d"
        );
    }

    #[test]
    fn types_and_intervals() {
        assert_eq!(SeriesType::Rate.as_str(), "rate");
        assert_eq!(SeriesType::Rate.interval(), Some(10));
        assert_eq!(SeriesType::Distribution.as_str(), "distribution");
        assert_eq!(SeriesType::Distribution.interval(), None);
    }

    #[test]
    fn a_nano_usd_total_is_divided_once() {
        // 21 nano-USD: 2.1e-9 USD over DogStatsD, 2.1e-10 USD/s over the API,
        // each one division of the exact sum. Dividing the USD total by 10
        // again would round twice.
        let value = Value::Counter {
            total: Sum::Integer(21),
            nano_usd: true,
        };
        assert_eq!(value.total(), 21.0 / 1e9);
        assert_eq!(value.api_value(), 21.0 / 1e10);
        assert_ne!(value.api_value(), (21.0 / 1e9) / 10.0);
    }
}
