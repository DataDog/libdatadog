// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! The points of one export window.
//!
//! An integration projects each call as it ends and adds the projection to a
//! [`MetricBatch`]. At each flush it encodes the batch for its exporter and
//! starts a new one. The batch is a plain value owned by the caller: the crate
//! keeps no state of its own.

use std::collections::BTreeMap;

use crate::Profile;
use crate::datadog::{DatadogSeries, datadog_series};
use crate::point::{
    Attributes, ErrorCode, Instrument, MetricError, MetricPoint, Projection, Sum, reject,
};
use crate::registry::PROFILE_ATTRIBUTE;

/// One series of a batch: a metric name and an attribute set.
pub(crate) type SeriesKey = (String, Attributes);

/// The points of one resource and one profile.
#[derive(Debug, Clone, Default)]
pub(crate) struct Group {
    /// Counter sums, exact while every increment is a whole number.
    pub(crate) counters: BTreeMap<SeriesKey, (String, Sum)>,
    /// Histogram samples, in the order they were recorded.
    pub(crate) histograms: BTreeMap<SeriesKey, (String, Vec<f64>)>,
}

impl Group {
    fn points(&self) -> Vec<MetricPoint> {
        let counters = self
            .counters
            .iter()
            .map(|((name, attributes), (unit, sum))| match sum {
                Sum::Integer(total) => {
                    MetricPoint::counter_exact(name, unit, *total, attributes.clone())
                }
                Sum::Double(total) => MetricPoint::counter(name, unit, *total, attributes.clone()),
            });
        let histograms =
            self.histograms
                .iter()
                .flat_map(|((name, attributes), (unit, samples))| {
                    samples.iter().map(move |sample| {
                        MetricPoint::histogram(name, unit, *sample, attributes.clone())
                    })
                });
        counters.chain(histograms).collect()
    }
}

/// Projections collected over one export window, grouped by resource and
/// profile. Counters are summed exactly as they are added; Histogram samples
/// are kept.
#[derive(Debug, Clone, Default)]
pub struct MetricBatch {
    pub(crate) groups: BTreeMap<(Attributes, Profile), Group>,
}

impl MetricBatch {
    pub fn new() -> Self {
        Self::default()
    }

    /// Add the points of one projection of `profile`. A point that is not a
    /// metric of the profile, or that states another profile, is
    /// `metric_profile_mismatch`, and nothing of the projection is added.
    pub fn add(&mut self, profile: Profile, projection: &Projection) -> Result<(), MetricError> {
        for point in &projection.points {
            if !profile.metric_names().contains(&point.name.as_str()) {
                return reject(
                    ErrorCode::MetricProfileMismatch,
                    format!("{} is not a metric of {}", point.name, profile.id()),
                );
            }
            if point
                .attributes
                .get(PROFILE_ATTRIBUTE)
                .is_some_and(|stated| stated != profile.id())
            {
                return reject(
                    ErrorCode::MetricProfileMismatch,
                    format!(
                        "{} states another profile than {}",
                        point.name,
                        profile.id()
                    ),
                );
            }
        }
        let group = self
            .groups
            .entry((projection.resource.clone(), profile))
            .or_default();
        for point in &projection.points {
            let key = (point.name.clone(), point.attributes.clone());
            match point.instrument {
                Instrument::Counter => {
                    let entry = group
                        .counters
                        .entry(key)
                        .or_insert_with(|| (point.unit.clone(), Sum::Integer(0)));
                    entry.1 = entry.1.add(Sum::of(point));
                }
                Instrument::Histogram => {
                    group
                        .histograms
                        .entry(key)
                        .or_insert_with(|| (point.unit.clone(), Vec::new()))
                        .1
                        .push(point.value);
                }
            }
        }
        Ok(())
    }

    pub fn is_empty(&self) -> bool {
        self.groups
            .values()
            .all(|group| group.counters.is_empty() && group.histograms.is_empty())
    }

    /// The batch as points: per resource and profile, one point per Counter
    /// series with its sum, and one point per Histogram sample.
    pub fn points(&self) -> Vec<(Attributes, Profile, Vec<MetricPoint>)> {
        self.groups
            .iter()
            .map(|((resource, profile), group)| (resource.clone(), *profile, group.points()))
            .collect()
    }

    /// The Datadog series of the batch, as one submission.
    pub fn datadog_series(&self, historical: bool) -> Result<Vec<DatadogSeries>, MetricError> {
        let mut series = Vec::new();
        for ((resource, profile), group) in &self.groups {
            series.extend(datadog_series(
                *profile,
                &group.points(),
                resource,
                historical,
            )?);
        }
        Ok(series)
    }

    /// The batch as DogStatsD lines, one per series point, without trailing
    /// newlines. Callers may join them with `\n` into datagrams of the size
    /// their transport allows.
    pub fn dogstatsd_lines(&self) -> Result<Vec<String>, MetricError> {
        Ok(self
            .datadog_series(false)?
            .iter()
            .map(DatadogSeries::to_dogstatsd)
            .collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Json, project_result};

    fn attempt(text: &str) -> Projection {
        project_result(Profile::ProviderAttempt, &Json::parse(text).unwrap()).unwrap()
    }

    const CALL: &str = r#"{"operation_name": "chat", "provider_name": "anthropic",
        "duration_seconds": 1.5, "streaming": false, "input_tokens": 10, "output_tokens": 2,
        "cost_usd": 0.1, "cost_source": "estimated", "observation_point": "gateway"}"#;

    #[test]
    fn counters_are_summed_exactly_and_samples_are_kept() {
        let mut batch = MetricBatch::new();
        assert!(batch.is_empty());
        let call = attempt(CALL);
        batch.add(Profile::ProviderAttempt, &call).unwrap();
        batch
            .add(
                Profile::ProviderAttempt,
                &attempt(&CALL.replace("0.1,", "0.2,")),
            )
            .unwrap();
        assert!(!batch.is_empty());
        let groups = batch.points();
        assert_eq!(groups.len(), 1);
        let points = &groups[0].2;
        let cost: Vec<&MetricPoint> = points
            .iter()
            .filter(|p| p.name == "trajectory.gen_ai.client.inference.usage.cost")
            .collect();
        assert_eq!(cost.len(), 1);
        assert_eq!(cost[0].exact_value, Some(300_000_000));
        let durations = points
            .iter()
            .filter(|p| p.name == "gen_ai.client.inference.duration")
            .count();
        assert_eq!(durations, 2);
    }

    #[test]
    fn dogstatsd_lines_carry_the_profile_and_usd() {
        let mut batch = MetricBatch::new();
        batch.add(Profile::ProviderAttempt, &attempt(CALL)).unwrap();
        batch
            .add(
                Profile::ProviderAttempt,
                &attempt(&CALL.replace("0.1,", "0.2,")),
            )
            .unwrap();
        let lines = batch.dogstatsd_lines().unwrap();
        let profile = "trajectory.profile:gen_ai.client.provider_attempt/0.1.0";
        assert!(lines.iter().all(|line| line.contains(profile)), "{lines:?}");
        assert!(
            lines
                .iter()
                .any(|line| line
                    .starts_with("trajectory.gen_ai.client.inference.usage.cost:0.3|c|#")),
            "{lines:?}"
        );
        assert!(
            lines
                .iter()
                .any(|line| line.starts_with("gen_ai.client.inference.duration.count:2|c|#")),
            "{lines:?}"
        );
        let samples = lines
            .iter()
            .filter(|line| line.starts_with("gen_ai.client.inference.duration:1.5|d|#"))
            .count();
        assert_eq!(samples, 2);
    }

    #[test]
    fn points_of_another_profile_are_refused_whole() {
        let mut batch = MetricBatch::new();
        let error = batch
            .add(Profile::GatewayRequest, &attempt(CALL))
            .unwrap_err();
        assert_eq!(error.code(), ErrorCode::MetricProfileMismatch);
        assert!(batch.is_empty());
    }
}
