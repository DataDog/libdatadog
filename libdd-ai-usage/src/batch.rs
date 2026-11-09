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
use crate::datadog::{DatadogSeries, SeriesBuilder};
use crate::point::{
    Attributes, ErrorCode, Instrument, MetricError, MetricPoint, Projection, Sum, reject,
};
use crate::registry::PROFILE_ATTRIBUTE;

/// One series of a batch: a metric name and an attribute set.
pub(crate) type SeriesKey = (String, Attributes);

/// The samples of one Histogram series, in the order they were recorded.
#[derive(Debug, Clone, Default)]
pub(crate) struct Samples {
    pub(crate) values: Vec<f64>,
    /// Each value's place in the batch's recording order, across groups.
    pub(crate) order: Vec<u64>,
}

/// The points of one resource and one profile.
#[derive(Debug, Clone, Default)]
pub(crate) struct Group {
    /// Counter sums, exact while every increment is a whole number.
    pub(crate) counters: BTreeMap<SeriesKey, (String, Sum)>,
    pub(crate) histograms: BTreeMap<SeriesKey, (String, Samples)>,
}

impl Group {
    fn counter_points(&self) -> Vec<MetricPoint> {
        self.counters
            .iter()
            .map(|((name, attributes), (unit, sum))| match sum {
                Sum::Integer(total) => {
                    MetricPoint::counter_exact(name, unit, *total, attributes.clone())
                }
                Sum::Double(total) => MetricPoint::counter(name, unit, *total, attributes.clone()),
            })
            .collect()
    }

    /// One point per Histogram sample, with its place in the batch's
    /// recording order.
    fn histogram_points(&self) -> impl Iterator<Item = (u64, MetricPoint)> + '_ {
        self.histograms
            .iter()
            .flat_map(|((name, attributes), (unit, samples))| {
                samples
                    .order
                    .iter()
                    .zip(&samples.values)
                    .map(move |(order, value)| {
                        (
                            *order,
                            MetricPoint::histogram(name, unit, *value, attributes.clone()),
                        )
                    })
            })
    }

    fn points(&self) -> Vec<MetricPoint> {
        let mut points = self.counter_points();
        points.extend(self.histogram_points().map(|(_, point)| point));
        points
    }
}

/// Projections collected over one export window, grouped by resource and
/// profile. Counters are summed exactly as they are added; Histogram samples
/// are kept.
#[derive(Debug, Clone, Default)]
pub struct MetricBatch {
    pub(crate) groups: BTreeMap<(Attributes, Profile), Group>,
    /// Histogram samples added so far.
    recorded: u64,
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
        // The profile is stated once per group, as the OTLP scope attribute; a
        // resource never states it, as in the Datadog binding.
        let mut resource = projection.resource.clone();
        resource.remove(PROFILE_ATTRIBUTE);
        let group = self.groups.entry((resource, profile)).or_default();
        for point in &projection.points {
            // The profile is stated once per group, as the OTLP scope
            // attribute; a point that also states it is the same series as
            // one that does not.
            let mut attributes = point.attributes.clone();
            attributes.remove(PROFILE_ATTRIBUTE);
            let key = (point.name.clone(), attributes);
            match point.instrument {
                Instrument::Counter => {
                    let entry = group
                        .counters
                        .entry(key)
                        .or_insert_with(|| (point.unit.clone(), Sum::Integer(0)));
                    entry.1 = entry.1.add(Sum::of(point));
                }
                Instrument::Histogram => {
                    let samples = &mut group
                        .histograms
                        .entry(key)
                        .or_insert_with(|| (point.unit.clone(), Samples::default()))
                        .1;
                    samples.values.push(point.value);
                    samples.order.push(self.recorded);
                    self.recorded += 1;
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

    /// The Datadog series of the batch, as one submission. Resources whose
    /// tags normalize to the same series share its Counts.
    pub fn datadog_series(&self, historical: bool) -> Result<Vec<DatadogSeries>, MetricError> {
        let mut builder = SeriesBuilder::default();
        let mut samples = Vec::new();
        for ((resource, profile), group) in &self.groups {
            builder.add(*profile, &group.counter_points(), resource, historical)?;
            samples.extend(
                group
                    .histogram_points()
                    .map(|(order, point)| (order, *profile, resource, point)),
            );
        }
        // A Histogram's `.sum` adds its samples in the order they were
        // recorded, also when they come from resources whose tags normalize
        // to the same series.
        samples.sort_unstable_by_key(|(order, ..)| *order);
        for (_, profile, resource, point) in samples {
            builder.add(profile, std::slice::from_ref(&point), resource, historical)?;
        }
        Ok(builder.finish())
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
    fn resources_with_the_same_datadog_tags_share_their_counts() {
        let mut upper = attempt(CALL);
        upper.resource.insert("service.name".into(), "API".into());
        let mut lower = attempt(&CALL.replace("0.1,", "0.2,"));
        lower.resource.insert("service.name".into(), "api".into());
        let mut batch = MetricBatch::new();
        batch.add(Profile::ProviderAttempt, &upper).unwrap();
        batch.add(Profile::ProviderAttempt, &lower).unwrap();
        let lines = batch.dogstatsd_lines().unwrap();
        let cost: Vec<&String> = lines
            .iter()
            .filter(|line| line.starts_with("trajectory.gen_ai.client.inference.usage.cost:"))
            .collect();
        assert_eq!(cost.len(), 1, "{lines:?}");
        assert!(
            cost[0].starts_with("trajectory.gen_ai.client.inference.usage.cost:0.3|c|#"),
            "{cost:?}"
        );
        assert!(cost[0].contains("service:api"), "{cost:?}");
    }

    #[test]
    fn histogram_sums_follow_the_recording_order_across_resources() {
        // Recorded API, api, API: grouped by resource, the sum would add
        // 0.2 + 0.4 + 0.3, which is not the same double.
        let mut batch = MetricBatch::new();
        for (service, duration) in [("API", "0.2"), ("api", "0.3"), ("API", "0.4")] {
            let mut call = attempt(&CALL.replace("1.5,", &format!("{duration},")));
            call.resource.insert("service.name".into(), service.into());
            batch.add(Profile::ProviderAttempt, &call).unwrap();
        }
        let series = batch.datadog_series(false).unwrap();
        let sums: Vec<&DatadogSeries> = series
            .iter()
            .filter(|s| s.name == "gen_ai.client.inference.duration.sum")
            .collect();
        assert_eq!(sums.len(), 1, "{sums:?}");
        assert_eq!(sums[0].value, 0.2 + 0.3 + 0.4);
        assert_ne!(sums[0].value, 0.2 + 0.4 + 0.3);
        let samples = series
            .iter()
            .filter(|s| s.name == "gen_ai.client.inference.duration")
            .count();
        assert_eq!(samples, 3);
    }

    #[test]
    fn a_resource_never_states_the_profile() {
        let mut stated = attempt(CALL);
        stated.resource.insert(
            PROFILE_ATTRIBUTE.into(),
            Profile::ProviderAttempt.id().into(),
        );
        let mut batch = MetricBatch::new();
        batch.add(Profile::ProviderAttempt, &stated).unwrap();
        batch.add(Profile::ProviderAttempt, &attempt(CALL)).unwrap();
        let groups = batch.points();
        assert_eq!(groups.len(), 1);
        assert!(!groups[0].0.contains_key(PROFILE_ATTRIBUTE));
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
