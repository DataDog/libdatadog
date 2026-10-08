// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! Metric points, issue codes, and error codes produced by the projectors.

use std::cmp::Ordering;
use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

use serde::{Serialize, Serializer};

use crate::json::Json;
use crate::number::{exact_integer, exact_u64, integer_to_f64};

/// Attributes of a point, in key order.
pub type Attributes = BTreeMap<String, String>;

/// The OpenTelemetry instrument a point belongs to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Instrument {
    Counter,
    Histogram,
}

impl Instrument {
    pub fn as_str(self) -> &'static str {
        match self {
            Instrument::Counter => "counter",
            Instrument::Histogram => "histogram",
        }
    }
}

/// One projected metric point: a histogram sample or a counter increment.
///
/// Attributes are kept in a sorted map, so equality does not depend on
/// insertion order. Integral values serialize as JSON integers, matching the
/// conformance fixtures.
///
/// Two points are equal when they are the same point of the interchange
/// format: `value` compares as a number, so `-0` equals `0`, and
/// `exact_value` is not compared.
#[derive(Debug, Clone, Serialize)]
pub struct MetricPoint {
    pub name: String,
    pub instrument: Instrument,
    pub unit: String,
    /// The value as a double. For a Counter with an `exact_value` it is the
    /// double nearest to that integer.
    #[serde(serialize_with = "serialize_value")]
    pub value: f64,
    pub attributes: Attributes,
    /// The exact value of a Counter whose increments are whole numbers. A
    /// sum of increments can pass 2^53, where `value` is only the nearest
    /// double. Points of one series are added on this integer. It is not
    /// part of the interchange format.
    #[serde(skip)]
    pub exact_value: Option<u128>,
}

impl PartialEq for MetricPoint {
    fn eq(&self, other: &Self) -> bool {
        self.name == other.name
            && self.instrument == other.instrument
            && self.unit == other.unit
            && self.value == other.value
            && self.attributes == other.attributes
    }
}

impl MetricPoint {
    pub fn histogram(name: &str, unit: &str, value: f64, attributes: Attributes) -> Self {
        Self {
            name: name.to_string(),
            instrument: Instrument::Histogram,
            unit: unit.to_string(),
            value,
            attributes,
            exact_value: None,
        }
    }

    /// A Counter point. A whole-number value is also kept as an integer.
    pub fn counter(name: &str, unit: &str, value: f64, attributes: Attributes) -> Self {
        Self {
            name: name.to_string(),
            instrument: Instrument::Counter,
            unit: unit.to_string(),
            value,
            attributes,
            exact_value: exact_integer(value),
        }
    }

    /// A Counter point with an exact integer value.
    pub fn counter_exact(name: &str, unit: &str, value: u128, attributes: Attributes) -> Self {
        Self {
            name: name.to_string(),
            instrument: Instrument::Counter,
            unit: unit.to_string(),
            value: integer_to_f64(value),
            attributes,
            exact_value: Some(value),
        }
    }

    /// The exact integer this point adds to a sum, when it has one:
    /// `exact_value` when it agrees with `value`, else `value` itself when
    /// it is a whole number.
    pub fn exact(&self) -> Option<u128> {
        self.exact_value
            .filter(|exact| integer_to_f64(*exact) == self.value)
            .or_else(|| exact_integer(self.value))
    }
}

impl MetricPoint {
    /// Read points in the format of the conformance cases:
    /// `[{"name", "instrument", "unit", "value", "attributes"}]`. A member of
    /// the wrong JSON type, or an unknown instrument, is `field_type_invalid`.
    pub fn list_from_json(points: &Json) -> Result<Vec<MetricPoint>, MetricError> {
        let read = |point: &Json| {
            let instrument = match point.get("instrument")?.as_str()? {
                "counter" => Instrument::Counter,
                "histogram" => Instrument::Histogram,
                _ => return None,
            };
            let attributes = point
                .get("attributes")?
                .as_object()?
                .iter()
                .map(|(key, value)| Some((key.clone(), value.as_str()?.to_string())))
                .collect::<Option<Attributes>>()?;
            let value = point.get("value")?.as_f64()?;
            Some(MetricPoint {
                name: point.get("name")?.as_str()?.to_string(),
                instrument,
                unit: point.get("unit")?.as_str()?.to_string(),
                value,
                attributes,
                exact_value: None,
            })
        };
        points
            .as_array()
            .and_then(|items| items.iter().map(read).collect::<Option<Vec<MetricPoint>>>())
            .ok_or_else(|| {
                MetricError::new(
                    ErrorCode::FieldTypeInvalid,
                    "points must be an array of metric points",
                )
            })
    }
}

fn serialize_value<S: Serializer>(value: &f64, serializer: S) -> Result<S::Ok, S::Error> {
    match exact_u64(*value) {
        Some(integer) => serializer.serialize_u64(integer),
        None => serializer.serialize_f64(*value),
    }
}

/// The closed list of rejection codes (`PORTABLE-METRICS.md#error-codes`).
/// Conformance compares these, never message text.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum ErrorCode {
    FieldTypeInvalid,
    ValueNotAllowed,
    IdentifierInvalid,
    CountInvalid,
    DurationInvalid,
    RequiredFieldMissing,
    FieldsContradict,
    SequenceInvalid,
    EventOrderInvalid,
    TimestampInvalid,
    DeploymentAttributeUnknown,
    ContextBandsInvalid,
    ProfileInvalid,
    MetricProfileMismatch,
}

impl ErrorCode {
    pub const ALL: [ErrorCode; 14] = [
        ErrorCode::FieldTypeInvalid,
        ErrorCode::ValueNotAllowed,
        ErrorCode::IdentifierInvalid,
        ErrorCode::CountInvalid,
        ErrorCode::DurationInvalid,
        ErrorCode::RequiredFieldMissing,
        ErrorCode::FieldsContradict,
        ErrorCode::SequenceInvalid,
        ErrorCode::EventOrderInvalid,
        ErrorCode::TimestampInvalid,
        ErrorCode::DeploymentAttributeUnknown,
        ErrorCode::ContextBandsInvalid,
        ErrorCode::ProfileInvalid,
        ErrorCode::MetricProfileMismatch,
    ];

    /// The stable string of the code.
    pub fn as_str(self) -> &'static str {
        match self {
            ErrorCode::FieldTypeInvalid => "field_type_invalid",
            ErrorCode::ValueNotAllowed => "value_not_allowed",
            ErrorCode::IdentifierInvalid => "identifier_invalid",
            ErrorCode::CountInvalid => "count_invalid",
            ErrorCode::DurationInvalid => "duration_invalid",
            ErrorCode::RequiredFieldMissing => "required_field_missing",
            ErrorCode::FieldsContradict => "fields_contradict",
            ErrorCode::SequenceInvalid => "sequence_invalid",
            ErrorCode::EventOrderInvalid => "event_order_invalid",
            ErrorCode::TimestampInvalid => "timestamp_invalid",
            ErrorCode::DeploymentAttributeUnknown => "deployment_attribute_unknown",
            ErrorCode::ContextBandsInvalid => "context_bands_invalid",
            ErrorCode::ProfileInvalid => "profile_invalid",
            ErrorCode::MetricProfileMismatch => "metric_profile_mismatch",
        }
    }
}

impl fmt::Display for ErrorCode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// The issue codes a valid projection can raise beside its points
/// (`PORTABLE-METRICS.md#issue-codes`). The derived order is the order of the
/// stable strings, so a sorted set of issues is sorted by code.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum IssueCode {
    CacheReadMissing,
    CacheWriteLifetimeUnknown,
    CacheWriteMissing,
    CorrectionKeyIgnored,
    CostRateCardDropped,
    CostUnusable,
    CountInvalid,
    CountOutOfRange,
    DeploymentAttributeDropped,
    DurationInvalid,
    FieldTypeInvalid,
    InputBasisMissing,
    InputTotalMissing,
    OutputBasisMissing,
    OutputTotalMissing,
    ServerToolCountConflict,
    UsageInconsistent,
    ValueNotAllowed,
}

impl IssueCode {
    pub const ALL: [IssueCode; 18] = [
        IssueCode::CacheReadMissing,
        IssueCode::CacheWriteLifetimeUnknown,
        IssueCode::CacheWriteMissing,
        IssueCode::CorrectionKeyIgnored,
        IssueCode::CostRateCardDropped,
        IssueCode::CostUnusable,
        IssueCode::CountInvalid,
        IssueCode::CountOutOfRange,
        IssueCode::DeploymentAttributeDropped,
        IssueCode::DurationInvalid,
        IssueCode::FieldTypeInvalid,
        IssueCode::InputBasisMissing,
        IssueCode::InputTotalMissing,
        IssueCode::OutputBasisMissing,
        IssueCode::OutputTotalMissing,
        IssueCode::ServerToolCountConflict,
        IssueCode::UsageInconsistent,
        IssueCode::ValueNotAllowed,
    ];

    /// The stable string of the code.
    pub fn as_str(self) -> &'static str {
        match self {
            IssueCode::CacheReadMissing => "cache_read_missing",
            IssueCode::CacheWriteLifetimeUnknown => "cache_write_lifetime_unknown",
            IssueCode::CacheWriteMissing => "cache_write_missing",
            IssueCode::CorrectionKeyIgnored => "correction_key_ignored",
            IssueCode::CostRateCardDropped => "cost_rate_card_dropped",
            IssueCode::CostUnusable => "cost_unusable",
            IssueCode::CountInvalid => "count_invalid",
            IssueCode::CountOutOfRange => "count_out_of_range",
            IssueCode::DeploymentAttributeDropped => "deployment_attribute_dropped",
            IssueCode::DurationInvalid => "duration_invalid",
            IssueCode::FieldTypeInvalid => "field_type_invalid",
            IssueCode::InputBasisMissing => "input_basis_missing",
            IssueCode::InputTotalMissing => "input_total_missing",
            IssueCode::OutputBasisMissing => "output_basis_missing",
            IssueCode::OutputTotalMissing => "output_total_missing",
            IssueCode::ServerToolCountConflict => "server_tool_count_conflict",
            IssueCode::UsageInconsistent => "usage_inconsistent",
            IssueCode::ValueNotAllowed => "value_not_allowed",
        }
    }
}

impl fmt::Display for IssueCode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl Serialize for IssueCode {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.as_str())
    }
}

/// Issue codes, sorted and without repeats.
pub type Issues = BTreeSet<IssueCode>;

/// A rejected input: a stable code and a diagnostic for people. Only the
/// code is part of the contract.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MetricError {
    code: ErrorCode,
    message: String,
}

impl MetricError {
    pub fn new(code: ErrorCode, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }

    pub fn code(&self) -> ErrorCode {
        self.code
    }

    pub fn message(&self) -> &str {
        &self.message
    }
}

impl fmt::Display for MetricError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.code.as_str(), self.message)
    }
}

impl std::error::Error for MetricError {}

pub(crate) fn reject<T>(code: ErrorCode, message: impl Into<String>) -> Result<T, MetricError> {
    Err(MetricError::new(code, message))
}

/// What a projection returns: the points, the issue codes raised beside
/// them, and the resource attributes that deployment attributes gave.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Projection {
    pub points: Vec<MetricPoint>,
    pub issues: Issues,
    pub resource: Attributes,
}

/// The conformance comparison order shared by every metric profile: metric
/// name, then attributes as key-sorted (key, value) pairs compared pair by
/// pair (a shorter prefix sorts first), then value.
pub fn compare_metric_points(a: &MetricPoint, b: &MetricPoint) -> Ordering {
    a.name
        .cmp(&b.name)
        .then_with(|| a.attributes.iter().cmp(b.attributes.iter()))
        .then_with(|| a.value.total_cmp(&b.value))
}

pub fn sort_metric_points(mut points: Vec<MetricPoint>) -> Vec<MetricPoint> {
    points.sort_by(compare_metric_points);
    points
}

/// The sum of one series.
#[derive(Clone, Copy)]
enum Sum {
    /// Every increment so far was a whole number: the sum is exact.
    Integer(u128),
    Double(f64),
}

impl Sum {
    fn of(point: &MetricPoint) -> Self {
        point.exact().map_or(Sum::Double(point.value), Sum::Integer)
    }

    fn value(self) -> f64 {
        match self {
            Sum::Integer(total) => integer_to_f64(total),
            Sum::Double(total) => total,
        }
    }

    /// Integers are added exactly, so the order of increments does not
    /// matter. An increment at most 2^128 cannot overflow the sum in
    /// practice; if it did, the sum would continue as a double.
    fn add(self, other: Sum) -> Sum {
        match (self, other) {
            (Sum::Integer(a), Sum::Integer(b)) => match a.checked_add(b) {
                Some(total) => Sum::Integer(total),
                None => Sum::Double(integer_to_f64(a) + integer_to_f64(b)),
            },
            (a, b) => Sum::Double(a.value() + b.value()),
        }
    }
}

/// Counter points aggregated per (name, attribute set), in first-seen order:
/// one point per identity. Whole-number increments are added as integers and
/// the sum is converted to a double once.
#[derive(Default)]
pub(crate) struct CounterSet {
    index: BTreeMap<(String, Attributes), usize>,
    entries: Vec<(String, String, Attributes, Sum)>,
}

impl CounterSet {
    fn add_sum(&mut self, name: &str, unit: &str, increment: Sum, attributes: Attributes) {
        let key = (name.to_string(), attributes);
        match self
            .index
            .get(&key)
            .and_then(|at| self.entries.get_mut(*at))
        {
            Some(entry) => entry.3 = entry.3.add(increment),
            None => {
                self.index.insert(key.clone(), self.entries.len());
                self.entries
                    .push((key.0, unit.to_string(), key.1, increment));
            }
        }
    }

    pub(crate) fn add_count(&mut self, name: &str, unit: &str, value: u64, attributes: Attributes) {
        self.add_sum(name, unit, Sum::Integer(u128::from(value)), attributes);
    }

    pub(crate) fn into_points(self) -> Vec<MetricPoint> {
        self.entries
            .into_iter()
            .map(|(name, unit, attributes, sum)| match sum {
                Sum::Integer(total) => MetricPoint::counter_exact(&name, &unit, total, attributes),
                Sum::Double(total) => MetricPoint::counter(&name, &unit, total, attributes),
            })
            .collect()
    }
}

/// One point per identity: Counter points with the same name and attribute
/// set become one point, at the place of the first, with the exact sum.
/// Histogram samples are never merged.
pub fn merge_counter_points(points: Vec<MetricPoint>) -> Vec<MetricPoint> {
    let mut first: BTreeMap<(String, Attributes), usize> = BTreeMap::new();
    let mut merged: Vec<(MetricPoint, Sum)> = Vec::with_capacity(points.len());
    for point in points {
        let sum = Sum::of(&point);
        if point.instrument != Instrument::Counter {
            merged.push((point, sum));
            continue;
        }
        let key = (point.name.clone(), point.attributes.clone());
        match first.get(&key).and_then(|at| merged.get_mut(*at)) {
            Some(entry) => entry.1 = entry.1.add(sum),
            None => {
                first.insert(key, merged.len());
                merged.push((point, sum));
            }
        }
    }
    merged
        .into_iter()
        .map(|(mut point, sum)| {
            if point.instrument == Instrument::Counter {
                point.value = sum.value();
                point.exact_value = match sum {
                    Sum::Integer(total) => Some(total),
                    Sum::Double(_) => None,
                };
            }
            point
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn codes_are_unique_stable_strings_and_issues_sort_by_string() {
        let errors: BTreeSet<&str> = ErrorCode::ALL.iter().map(|c| c.as_str()).collect();
        assert_eq!(errors.len(), ErrorCode::ALL.len());
        let issues: Vec<&str> = IssueCode::ALL.iter().map(|c| c.as_str()).collect();
        let mut sorted = issues.clone();
        sorted.sort_unstable();
        sorted.dedup();
        assert_eq!(issues, sorted);
        let mut by_enum = IssueCode::ALL.to_vec();
        by_enum.reverse();
        by_enum.sort();
        assert_eq!(by_enum, IssueCode::ALL);
    }

    #[test]
    fn counter_sums_are_exact_in_any_order() {
        // Five increments that sum to 2^53 + 2. Added one by one as doubles
        // in this order they give 2^53.
        let increments = [9_007_199_254_740_991_u64, 1, 1, 1, 0];
        let as_doubles = increments
            .iter()
            .fold(0.0, |sum, n| sum + integer_to_f64(u128::from(*n)));
        assert_eq!(as_doubles, 9_007_199_254_740_992.0);
        for rotation in 0..increments.len() {
            let mut set = CounterSet::default();
            let mut order = increments;
            order.rotate_left(rotation);
            for increment in order {
                set.add_count("m", "{token}", increment, Attributes::new());
            }
            let points = set.into_points();
            assert_eq!(points.len(), 1);
            assert_eq!(points[0].value, 9_007_199_254_740_994.0, "{order:?}");
            assert_eq!(points[0].exact_value, Some(9_007_199_254_740_994));
        }
    }

    #[test]
    fn an_exact_sum_with_no_double_keeps_its_integer_through_a_merge() {
        // 2^53 + 1 has no double: its point value is 2^53. Adding 1 to that
        // double changes nothing; adding 1 to the integer gives 2^53 + 2.
        let odd = (1_u128 << 53) + 1;
        let a = MetricPoint::counter_exact("m", "{nanoUSD}", odd, Attributes::new());
        let b = MetricPoint::counter_exact("m", "{nanoUSD}", 1, Attributes::new());
        assert_eq!(a.value, 9_007_199_254_740_992.0);
        assert_eq!(a.value + b.value, 9_007_199_254_740_992.0);
        for points in [vec![a.clone(), b.clone()], vec![b, a]] {
            let merged = merge_counter_points(points);
            assert_eq!(merged.len(), 1);
            assert_eq!(merged[0].exact_value, Some(odd + 1));
            assert_eq!(merged[0].value, 9_007_199_254_740_994.0);
        }
    }

    #[test]
    fn only_counter_points_of_one_identity_are_merged() {
        let mut other = Attributes::new();
        other.insert("k".into(), "v".into());
        let merged = merge_counter_points(vec![
            MetricPoint::histogram("h", "s", 1.0, Attributes::new()),
            MetricPoint::counter("c", "{x}", 1.0, Attributes::new()),
            MetricPoint::histogram("h", "s", 1.0, Attributes::new()),
            MetricPoint::counter("c", "{x}", 2.0, other),
            MetricPoint::counter("c", "{x}", 4.0, Attributes::new()),
            MetricPoint::counter("d", "{x}", 0.5, Attributes::new()),
            MetricPoint::counter("d", "{x}", 0.25, Attributes::new()),
        ]);
        let values: Vec<(&str, f64)> = merged.iter().map(|p| (p.name.as_str(), p.value)).collect();
        assert_eq!(
            values,
            [("h", 1.0), ("c", 5.0), ("h", 1.0), ("c", 2.0), ("d", 0.75)]
        );
        // A value changed by hand wins over a stale exact value.
        let mut edited = MetricPoint::counter("c", "{x}", 1.0, Attributes::new());
        edited.value = 7.0;
        assert_eq!(edited.exact(), Some(7));
    }

    #[test]
    fn point_values_serialize_as_integers_when_whole() {
        let point = MetricPoint::counter("m", "{token}", 12.0, Attributes::new());
        let text = serde_json::to_string(&point).unwrap();
        assert!(text.contains("\"value\":12,"), "{text}");
        let point = MetricPoint::histogram("m", "s", 0.5, Attributes::new());
        assert!(
            serde_json::to_string(&point)
                .unwrap()
                .contains("\"value\":0.5,")
        );
    }
}
