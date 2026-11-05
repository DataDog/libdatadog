// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! The points of one export window.
//!
//! An integration projects each call as it ends and adds the projection to a
//! [`MetricBatch`]. At each flush it encodes the batch for its exporter and
//! starts a new one. The batch is a plain value owned by the caller: the crate
//! keeps no state of its own.

use std::collections::{BTreeMap, BTreeSet};

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
    /// The points held: Counter series plus Histogram samples.
    len: usize,
    /// The most points the batch holds, if bounded.
    max_len: Option<usize>,
    /// Projections refused because the batch was full, since it was created.
    dropped: u64,
}

impl MetricBatch {
    pub fn new() -> Self {
        Self::default()
    }

    /// An empty batch that holds at most `max_len` points (see
    /// [`MetricBatch::len`]). A projection that would take it past the bound
    /// is dropped whole and counted by [`MetricBatch::dropped`].
    pub fn with_max_len(max_len: usize) -> Self {
        Self {
            max_len: Some(max_len),
            ..Self::default()
        }
    }

    /// The bound set by [`MetricBatch::with_max_len`] or
    /// [`MetricBatch::set_max_len`], if any.
    pub fn max_len(&self) -> Option<usize> {
        self.max_len
    }

    /// Bound the batch to `max_len` points, or lift the bound with `None`.
    /// Points already held are kept.
    pub fn set_max_len(&mut self, max_len: Option<usize>) {
        self.max_len = max_len;
    }

    /// The projections dropped because the batch was full, since it was
    /// created. [`MetricBatch::take`] keeps the count, so it only grows.
    pub fn dropped(&self) -> u64 {
        self.dropped
    }

    /// Take the points of the batch, leaving it empty with the same bound
    /// and dropped count.
    pub fn take(&mut self) -> MetricBatch {
        let empty = MetricBatch {
            max_len: self.max_len,
            dropped: self.dropped,
            ..MetricBatch::default()
        };
        std::mem::replace(self, empty)
    }

    /// Add the points of one projection of `profile`. A point that is not a
    /// metric of the profile, or that states another profile, is
    /// `metric_profile_mismatch`, and nothing of the projection is added. A
    /// projection that does not fit a bounded batch is dropped, as
    /// [`MetricBatch::try_add`] reports.
    pub fn add(&mut self, profile: Profile, projection: &Projection) -> Result<(), MetricError> {
        self.try_add(profile, projection).map(|_| ())
    }

    /// [`MetricBatch::add`], returning whether the projection was added:
    /// `false` when it would take a bounded batch past its bound, in which
    /// case nothing of it is added and [`MetricBatch::dropped`] counts it.
    pub fn try_add(
        &mut self,
        profile: Profile,
        projection: &Projection,
    ) -> Result<bool, MetricError> {
        self.try_add_owned(profile, projection.clone())
    }

    /// [`MetricBatch::try_add`] for a projection the caller no longer needs:
    /// its names and attribute sets become the batch's series keys as they
    /// are, without a copy per point.
    pub fn try_add_owned(
        &mut self,
        profile: Profile,
        projection: Projection,
    ) -> Result<bool, MetricError> {
        self.try_add_all([(profile, projection)])
    }

    /// Add the projections of one request (its provider attempts, token
    /// breakdowns and gateway request) together: all of them, or, when they
    /// would take a bounded batch past its bound, none, so a full batch drops
    /// whole requests and never keeps part of one. Returns whether they were
    /// added; [`MetricBatch::dropped`] counts each projection dropped. A
    /// projection that [`MetricBatch::add`] refuses refuses them all, and
    /// nothing is added.
    pub fn try_add_all(
        &mut self,
        projections: impl IntoIterator<Item = (Profile, Projection)>,
    ) -> Result<bool, MetricError> {
        let prepared = projections
            .into_iter()
            .map(|(profile, projection)| prepare(profile, projection))
            .collect::<Result<Vec<Prepared>, MetricError>>()?;
        if let Some(max_len) = self.max_len
            && self.len + self.added(&prepared) > max_len
        {
            self.dropped += prepared.len() as u64;
            return Ok(false);
        }
        for item in prepared {
            self.insert(item);
        }
        Ok(true)
    }

    /// The points `items` would add: their Histogram samples, and their
    /// Counter series that neither the batch nor an earlier item holds.
    fn added(&self, items: &[Prepared]) -> usize {
        let mut new_counters = BTreeSet::new();
        let mut added = 0;
        for item in items {
            let existing = self.groups.get(&item.group);
            for (key, instrument, ..) in &item.points {
                match instrument {
                    Instrument::Counter => {
                        let held = existing.is_some_and(|g| g.counters.contains_key(key));
                        if !held && new_counters.insert((&item.group, key)) {
                            added += 1;
                        }
                    }
                    Instrument::Histogram => added += 1,
                }
            }
        }
        added
    }

    fn insert(&mut self, item: Prepared) {
        let group = self.groups.entry(item.group).or_default();
        for (key, instrument, unit, sum, value) in item.points {
            match instrument {
                Instrument::Counter => {
                    let entry = group.counters.entry(key).or_insert_with(|| {
                        self.len += 1;
                        (unit, Sum::Integer(0))
                    });
                    entry.1 = entry.1.add(sum);
                }
                Instrument::Histogram => {
                    let samples = &mut group
                        .histograms
                        .entry(key)
                        .or_insert_with(|| (unit, Samples::default()))
                        .1;
                    samples.values.push(value);
                    samples.order.push(self.recorded);
                    self.recorded += 1;
                    self.len += 1;
                }
            }
        }
    }

    pub fn is_empty(&self) -> bool {
        self.groups
            .values()
            .all(|group| group.counters.is_empty() && group.histograms.is_empty())
    }

    /// The number of points the batch holds: one per Counter series and one
    /// per Histogram sample, as [`MetricBatch::points`] returns them. Series
    /// of resources whose Datadog tags normalize alike are still counted
    /// apart, so the DogStatsD lines can be fewer, never more. For a flush
    /// threshold; it encodes nothing.
    pub fn len(&self) -> usize {
        self.len
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

/// A projection checked and keyed for a batch, not added yet.
struct Prepared {
    group: (Attributes, Profile),
    points: Vec<(SeriesKey, Instrument, String, Sum, f64)>,
}

/// Check a projection of `profile` as [`MetricBatch::add`] does, and key its
/// points as the batch holds them.
fn prepare(profile: Profile, projection: Projection) -> Result<Prepared, MetricError> {
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
    let mut resource = projection.resource;
    resource.remove(PROFILE_ATTRIBUTE);
    let points: Vec<(SeriesKey, Instrument, String, Sum, f64)> = projection
        .points
        .into_iter()
        .map(|point| {
            let sum = Sum::of(&point);
            let MetricPoint {
                name,
                instrument,
                unit,
                value,
                mut attributes,
                ..
            } = point;
            // A point that also states the profile is the same series as
            // one that does not.
            attributes.remove(PROFILE_ATTRIBUTE);
            ((name, attributes), instrument, unit, sum, value)
        })
        .collect();
    Ok(Prepared {
        group: (resource, profile),
        points,
    })
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
        // One point per Counter series, one per Histogram sample.
        assert_eq!(batch.len(), points.len());
        assert_eq!(MetricBatch::new().len(), 0);
    }

    #[test]
    fn a_bounded_batch_drops_what_does_not_fit_and_counts_it() {
        let call = attempt(CALL);
        let mut unbounded = MetricBatch::new();
        unbounded.add(Profile::ProviderAttempt, &call).unwrap();
        let one = unbounded.len();
        unbounded.add(Profile::ProviderAttempt, &call).unwrap();
        // Counters merge into their series; only Histogram samples are new.
        let growth = unbounded.len() - one;
        assert!(growth > 0 && growth < one);

        let mut batch = MetricBatch::with_max_len(one + growth);
        assert_eq!(batch.max_len(), Some(one + growth));
        assert!(batch.try_add(Profile::ProviderAttempt, &call).unwrap());
        assert!(batch.try_add(Profile::ProviderAttempt, &call).unwrap());
        assert_eq!(batch.len(), one + growth);
        // A third call would exceed the bound: dropped whole, and counted.
        assert!(!batch.try_add(Profile::ProviderAttempt, &call).unwrap());
        batch.add(Profile::ProviderAttempt, &call).unwrap();
        assert_eq!(batch.len(), one + growth);
        assert_eq!(batch.dropped(), 2);
        // Taking empties the batch and keeps the bound and the count.
        let taken = batch.take();
        assert_eq!(taken.len(), one + growth);
        assert_eq!(batch.len(), 0);
        assert!(batch.is_empty());
        assert_eq!(batch.max_len(), Some(one + growth));
        assert_eq!(batch.dropped(), 2);
        assert!(batch.try_add(Profile::ProviderAttempt, &call).unwrap());
        // The cached length agrees with the points.
        let points: usize = batch.points().iter().map(|(_, _, p)| p.len()).sum();
        assert_eq!(batch.len(), points);
        batch.set_max_len(None);
        assert_eq!(batch.max_len(), None);
    }

    #[test]
    fn a_full_batch_drops_whole_requests() {
        let call = attempt(CALL);
        let breakdown = project_result(
            Profile::TokenBreakdown,
            &Json::parse(
                r#"{"operation_name": "chat", "provider_name": "anthropic",
                "input_tokens": 100, "input_basis": "excludes_cache", "output_tokens": 50,
                "output_basis": "includes_reasoning", "cache_read_input_tokens": 400,
                "cache_write_input_tokens": 20, "reasoning_output_tokens": 10,
                "observation_point": "gateway"}"#,
            )
            .unwrap(),
        )
        .unwrap();
        let request = || {
            vec![
                (Profile::ProviderAttempt, call.clone()),
                (Profile::TokenBreakdown, breakdown.clone()),
            ]
        };
        let mut unbounded = MetricBatch::new();
        assert!(unbounded.try_add_all(request()).unwrap());
        let one = unbounded.len();
        assert!(one > call.points.len());

        // Room for the attempt but not its breakdown: neither is added.
        let mut batch = MetricBatch::with_max_len(one - 1);
        assert!(!batch.try_add_all(request()).unwrap());
        assert!(batch.is_empty());
        assert_eq!(batch.dropped(), 2);
        batch.set_max_len(Some(one));
        assert!(batch.try_add_all(request()).unwrap());
        assert_eq!(batch.len(), one);

        // Series shared within the request are counted once.
        let mut twice = MetricBatch::new();
        twice
            .try_add_all([
                (Profile::ProviderAttempt, call.clone()),
                (Profile::ProviderAttempt, call.clone()),
            ])
            .unwrap();
        let mut exact = MetricBatch::with_max_len(twice.len());
        assert!(
            exact
                .try_add_all([
                    (Profile::ProviderAttempt, call.clone()),
                    (Profile::ProviderAttempt, call.clone()),
                ])
                .unwrap()
        );

        // One refused projection refuses the request.
        let mut batch = MetricBatch::new();
        let error = batch
            .try_add_all([
                (Profile::ProviderAttempt, call.clone()),
                (Profile::GatewayRequest, call.clone()),
            ])
            .unwrap_err();
        assert_eq!(error.code(), ErrorCode::MetricProfileMismatch);
        assert!(batch.is_empty());
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
