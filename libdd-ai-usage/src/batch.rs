// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! The points of one export window.
//!
//! An integration projects each call as it ends and adds the projection to a
//! [`MetricBatch`]. At each flush it encodes the batch for its exporter and
//! starts a new one. The batch is a plain value owned by the caller: the crate
//! keeps no state of its own.
//!
//! Series are kept in the order they were first added, behind a hash index,
//! so adding a request to series the batch already holds allocates nothing.
//! Takes read them sorted by resource, profile, name and attributes.

use std::hash::{BuildHasher, Hash, Hasher};
use std::ops::Range;

use hashbrown::{DefaultHashBuilder, HashTable};

use crate::PROFILE_ATTRIBUTE;
use crate::Profile;
use crate::datadog::{DatadogSeries, SeriesBuilder};
use crate::deployment::{Carrier, deployment_attribute_carrier, is_deployment_value};
use crate::observation::{
    GatewayRequestObservation, ProviderAttemptObservation, ProviderStreamingObservation,
    TokenBreakdownObservation,
};
use crate::point::{
    Attributes, Attrs, ErrorCode, Instrument, IssueCode, Issues, MetricError, MetricPoint,
    Projection, Sink, Sum, reject, same_key,
};
use crate::project;

/// The samples of one Histogram series, in the order they were recorded.
#[derive(Debug, Clone, Default)]
pub(crate) struct Samples {
    pub(crate) values: Vec<f64>,
    /// Each value's place in the batch's recording order, across series.
    pub(crate) order: Vec<u64>,
}

/// What one series holds.
#[derive(Debug, Clone)]
pub(crate) enum Data {
    /// Counter sum, exact while every increment is a whole number.
    Counter(Sum),
    Histogram(Samples),
}

/// One series of a batch: a resource and profile, a metric name and an
/// attribute set.
#[derive(Debug, Clone)]
pub(crate) struct Series {
    pub(crate) resource: usize,
    pub(crate) name: String,
    pub(crate) unit: String,
    pub(crate) attributes: Attributes,
    /// The attributes in the order they were first added, which the same
    /// projector repeats: compared before `attributes`.
    emitted: Vec<(String, String)>,
    pub(crate) data: Data,
}

/// The series of one resource and profile, sorted by name and attributes.
pub(crate) struct Group<'b> {
    pub(crate) resource: &'b Attributes,
    pub(crate) profile: Profile,
    pub(crate) series: Vec<&'b Series>,
}

/// Projections collected over one export window, grouped by resource and
/// profile. Counters are summed exactly as they are added; Histogram samples
/// are kept.
#[derive(Debug, Clone, Default)]
pub struct MetricBatch {
    resources: Vec<(Attributes, Profile)>,
    /// `(hash, index)` of each resource.
    resource_index: HashTable<(u64, usize)>,
    pub(crate) series: Vec<Series>,
    /// `(hash, index)` of each series.
    series_index: HashTable<(u64, usize)>,
    hasher: DefaultHashBuilder,
    /// Histogram samples added so far.
    recorded: u64,
    /// The points held: Counter series plus Histogram samples.
    len: usize,
    /// The most points the batch holds, if bounded.
    max_len: Option<usize>,
    /// Projections refused because the batch was full, since it was created.
    dropped: u64,
    /// The part of `dropped` already returned by [`MetricBatch::take_dropped`].
    dropped_reported: u64,
    /// The earliest second the next intake take may stamp.
    pub(crate) next_second: i64,
    /// This writer's `host.name` and `process.pid` tags, for intake takes.
    pub(crate) writer_tags: Vec<String>,
}

/// An observation of one of the profiles, borrowed.
#[derive(Debug, Clone, Copy)]
pub enum Observation<'a> {
    ProviderAttempt(&'a ProviderAttemptObservation),
    ProviderStreaming(&'a ProviderStreamingObservation),
    TokenBreakdown(&'a TokenBreakdownObservation),
    GatewayRequest(&'a GatewayRequestObservation),
}

impl Observation<'_> {
    pub fn profile(&self) -> Profile {
        match self {
            Observation::ProviderAttempt(_) => Profile::ProviderAttempt,
            Observation::ProviderStreaming(_) => Profile::ProviderStreaming,
            Observation::TokenBreakdown(_) => Profile::TokenBreakdown,
            Observation::GatewayRequest(_) => Profile::GatewayRequest,
        }
    }
}

/// One document of a request, as [`MetricBatch::try_add_request`] projects
/// it: an observation, its deployment attributes (a `None` value is one that
/// is not a string), and the metrics to keep (`None` for all).
#[derive(Debug, Clone, Copy)]
pub struct Document<'a> {
    pub observation: Observation<'a>,
    pub deployment_attributes: &'a [(&'a str, Option<&'a str>)],
    pub metrics: Option<&'a [&'a str]>,
}

/// A rejected document of a request: its index and why.
#[derive(Debug, Clone, PartialEq)]
pub struct DocumentError {
    pub document: usize,
    pub error: MetricError,
}

/// The points of one request, keyed, not added yet. Every string is
/// borrowed from the request's observations and attributes.
#[derive(Default)]
struct Pending<'a> {
    /// Attribute pairs, each set sorted by key.
    pairs: Vec<(&'a str, &'a str)>,
    /// Per document: its profile, its resource pairs and their hash.
    resources: Vec<(Profile, Range<usize>, u64)>,
    points: Vec<PendingPoint<'a>>,
}

struct PendingPoint<'a> {
    resource: usize,
    name: &'a str,
    unit: &'a str,
    instrument: Instrument,
    /// A Histogram sample.
    value: f64,
    /// A Counter increment.
    sum: Sum,
    attributes: Range<usize>,
    hash: u64,
}

impl Pending<'_> {
    fn with_capacity(documents: usize) -> Self {
        Self {
            pairs: Vec::with_capacity(16 * 8 * documents),
            resources: Vec::with_capacity(documents),
            points: Vec::with_capacity(12 * documents),
        }
    }

    /// Whether two points are the same series of the same resource.
    fn same_series(&self, a: &PendingPoint<'_>, b: &PendingPoint<'_>) -> bool {
        let (pa, ra, ha) = &self.resources[a.resource];
        let (pb, rb, hb) = &self.resources[b.resource];
        a.hash == b.hash
            && ha == hb
            && pa == pb
            && a.name == b.name
            && same_set(
                &self.pairs[a.attributes.clone()],
                &self.pairs[b.attributes.clone()],
            )
            && self.pairs[ra.clone()] == self.pairs[rb.clone()]
    }
}

fn sum_of_counter(value: f64) -> Sum {
    crate::number::exact_integer(value).map_or(Sum::Double(value), Sum::Integer)
}

fn same_pairs<'p>(
    map: &Attributes,
    pairs: impl ExactSizeIterator<Item = &'p (&'p str, &'p str)>,
) -> bool {
    map.len() == pairs.len()
        && map
            .iter()
            .zip(pairs)
            .all(|((k, v), (pk, pv))| k == pk && v == pv)
}

/// Whether a series has the attribute set `pairs`, which may be in any
/// order: positionally against the order it was first added in, else as a
/// set.
fn same_attributes(series: &Series, pairs: &[(&str, &str)]) -> bool {
    let positional = series.emitted.len() == pairs.len()
        && series
            .emitted
            .iter()
            .zip(pairs)
            .all(|((k, v), (pk, pv))| same_key(k, pk) && v == pv);
    positional
        || (series.attributes.len() == pairs.len()
            && pairs
                .iter()
                .all(|(k, v)| series.attributes.get(*k).is_some_and(|held| held == v)))
}

/// Whether two attribute sets, each key once, are equal in any order.
fn same_set(a: &[(&str, &str)], b: &[(&str, &str)]) -> bool {
    a.len() == b.len()
        && (a
            .iter()
            .zip(b)
            .all(|(x, y)| same_key(x.0, y.0) && x.1 == y.1)
            || a.iter()
                .all(|x| b.iter().any(|y| same_key(x.0, y.0) && x.1 == y.1)))
}

fn hash_pairs<'p>(state: &mut impl Hasher, pairs: impl Iterator<Item = (&'p str, &'p str)>) {
    for (key, value) in pairs {
        key.hash(state);
        value.hash(state);
    }
}

/// A projector writing one document's points into a [`Pending`] request.
struct PendingSink<'r, 'a> {
    pending: &'r mut Pending<'a>,
    hasher: &'r DefaultHashBuilder,
    document: usize,
    /// The document's point-carried deployment attributes.
    carried: &'r [(&'a str, &'a str)],
    metrics: Option<&'r [&'a str]>,
}

impl<'a> PendingSink<'_, 'a> {
    fn add(
        &mut self,
        name: &'a str,
        instrument: Instrument,
        unit: &'a str,
        (value, sum): (f64, Sum),
        own: impl Iterator<Item = (&'a str, &'a str)>,
    ) {
        if self.metrics.is_some_and(|metrics| !metrics.contains(&name)) {
            return;
        }
        let start = self.pending.pairs.len();
        // A profile attribute on a point is the series without it.
        self.pending
            .pairs
            .extend(own.filter(|(key, _)| *key != PROFILE_ATTRIBUTE));
        // A deployment attribute never replaces the projector's own.
        for &(key, value) in self.carried {
            if !self.pending.pairs[start..]
                .iter()
                .any(|(k, _)| same_key(k, key))
            {
                self.pending.pairs.push((key, value));
            }
        }
        // The pairs hash the same in any order, so they are not sorted.
        let mut pairs_hash = 0u64;
        for (key, value) in &self.pending.pairs[start..] {
            let mut pair = self.hasher.build_hasher();
            key.hash(&mut pair);
            value.hash(&mut pair);
            pairs_hash = pairs_hash.wrapping_add(pair.finish());
        }
        let resource_hash = self.pending.resources[self.document].2;
        let mut state = self.hasher.build_hasher();
        resource_hash.hash(&mut state);
        name.hash(&mut state);
        pairs_hash.hash(&mut state);
        let end = self.pending.pairs.len();
        self.pending.points.push(PendingPoint {
            resource: self.document,
            name,
            unit,
            instrument,
            value,
            sum,
            attributes: start..end,
            hash: state.finish(),
        });
    }
}

impl<'a> Sink<'a> for PendingSink<'_, 'a> {
    fn point(
        &mut self,
        name: &'static str,
        instrument: Instrument,
        unit: &'static str,
        value: f64,
        attributes: &Attrs<'a>,
    ) {
        let sum = match instrument {
            Instrument::Counter => sum_of_counter(value),
            Instrument::Histogram => Sum::Integer(0),
        };
        self.add(
            name,
            instrument,
            unit,
            (value, sum),
            attributes.pairs().iter().copied(),
        );
    }
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

    /// The projections dropped since the last call (since the batch was
    /// created, for the first), so a caller reports drops per flush without
    /// keeping a count of its own.
    pub(crate) fn take_dropped(&mut self) -> u64 {
        let since = self.dropped - self.dropped_reported;
        self.dropped_reported = self.dropped;
        since
    }

    /// Take the points of the batch, leaving it empty with the same bound,
    /// dropped count, writer and next intake second.
    pub fn take(&mut self) -> MetricBatch {
        let empty = MetricBatch {
            max_len: self.max_len,
            dropped: self.dropped,
            dropped_reported: self.dropped_reported,
            next_second: self.next_second,
            writer_tags: self.writer_tags.clone(),
            hasher: self.hasher.clone(),
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
        self.try_add_all([(profile, projection)])
    }

    /// [`MetricBatch::try_add`] for a projection the caller no longer needs.
    pub fn try_add_owned(
        &mut self,
        profile: Profile,
        projection: Projection,
    ) -> Result<bool, MetricError> {
        self.try_add_all([(profile, &projection)])
    }

    /// Add the projections of one request (its provider attempts, token
    /// breakdowns and gateway request) together: all of them, or, when they
    /// would take a bounded batch past its bound, none, so a full batch drops
    /// whole requests and never keeps part of one. Returns whether they were
    /// added; [`MetricBatch::dropped`] counts each projection dropped. A
    /// projection that [`MetricBatch::add`] refuses refuses them all, and
    /// nothing is added.
    pub fn try_add_all<P: std::borrow::Borrow<Projection>>(
        &mut self,
        projections: impl IntoIterator<Item = (Profile, P)>,
    ) -> Result<bool, MetricError> {
        let projections: Vec<(Profile, P)> = projections.into_iter().collect();
        let mut pending = Pending::default();
        for (document, (profile, projection)) in projections.iter().enumerate() {
            let projection = projection.borrow();
            check_profile(*profile, projection)?;
            self.open_resource(
                &mut pending,
                *profile,
                projection
                    .resource
                    .iter()
                    .filter(|(key, _)| key.as_str() != PROFILE_ATTRIBUTE)
                    .map(|(k, v)| (k.as_str(), v.as_str())),
            );
            let mut sink = PendingSink {
                pending: &mut pending,
                hasher: &self.hasher,
                document,
                carried: &[],
                metrics: None,
            };
            for point in &projection.points {
                sink.add(
                    &point.name,
                    point.instrument,
                    &point.unit,
                    (point.value, Sum::of(point)),
                    point
                        .attributes
                        .iter()
                        .map(|(k, v)| (k.as_str(), v.as_str())),
                );
            }
        }
        Ok(self.commit(&pending))
    }

    /// Project the documents of one request and add their points together,
    /// as [`MetricBatch::try_add_all`] adds projections, without building a
    /// [`Projection`] or an attribute map for a series the batch already
    /// holds. Deployment attributes are applied as
    /// [`crate::with_deployment_attributes`] applies them. Returns whether
    /// the request was added, and each document's issues.
    pub fn try_add_request(
        &mut self,
        documents: &[Document<'_>],
    ) -> Result<(bool, Vec<Issues>), DocumentError> {
        let mut pending = Pending::with_capacity(documents.len());
        let mut all_issues = Vec::with_capacity(documents.len());
        let mut carried = Vec::new();
        for (index, document) in documents.iter().enumerate() {
            let failed = |error| DocumentError {
                document: index,
                error,
            };
            let mut issues = Issues::default();
            let mut resource: Vec<(&str, &str)> = Vec::new();
            carried.clear();
            // An unknown name rejects the document once it has been
            // projected, so a rejected observation is reported first.
            let mut unknown = None;
            for &(name, value) in document.deployment_attributes {
                let Some(carrier) = deployment_attribute_carrier(name) else {
                    unknown = unknown.or(Some(name));
                    continue;
                };
                if unknown.is_some() {
                    continue;
                }
                let Some(value) = value.filter(|value| is_deployment_value(value)) else {
                    issues.insert(IssueCode::DeploymentAttributeDropped);
                    continue;
                };
                match carrier {
                    Carrier::Resource => match resource.iter_mut().find(|(k, _)| *k == name) {
                        Some(pair) => pair.1 = value,
                        None => resource.push((name, value)),
                    },
                    Carrier::Point => {
                        if !carried.iter().any(|(k, _): &(&str, &str)| *k == name) {
                            carried.push((name, value));
                        }
                    }
                }
            }
            resource.sort_unstable_by(|a, b| a.0.cmp(b.0));
            self.open_resource(
                &mut pending,
                document.observation.profile(),
                resource.into_iter(),
            );
            let mut sink = PendingSink {
                pending: &mut pending,
                hasher: &self.hasher,
                document: index,
                carried: &carried,
                metrics: document.metrics,
            };
            let projected = match document.observation {
                Observation::ProviderAttempt(o) => project::provider_attempt(o, &mut sink),
                Observation::ProviderStreaming(o) => project::provider_streaming(o, &mut sink),
                Observation::TokenBreakdown(o) => project::token_breakdown(o, &mut sink),
                Observation::GatewayRequest(o) => project::gateway_request(o, &mut sink),
            };
            let own = projected.map_err(failed)?;
            if let Some(name) = unknown {
                return Err(failed(MetricError::new(
                    ErrorCode::DeploymentAttributeUnknown,
                    format!("{name} is not a deployment attribute"),
                )));
            }
            issues.extend(own);
            all_issues.push(issues);
        }
        Ok((self.commit(&pending), all_issues))
    }

    /// Start a document's resource in `pending`. `pairs` are sorted by key.
    fn open_resource<'a>(
        &self,
        pending: &mut Pending<'a>,
        profile: Profile,
        pairs: impl Iterator<Item = (&'a str, &'a str)>,
    ) {
        let start = pending.pairs.len();
        pending.pairs.extend(pairs);
        let end = pending.pairs.len();
        let mut state = self.hasher.build_hasher();
        profile.hash(&mut state);
        hash_pairs(&mut state, pending.pairs[start..end].iter().copied());
        pending
            .resources
            .push((profile, start..end, state.finish()));
    }

    fn find_resource(&self, pending: &Pending<'_>, document: usize) -> Option<usize> {
        let (profile, range, hash) = &pending.resources[document];
        self.resource_index
            .find(*hash, |(_, at)| {
                let (attributes, held) = &self.resources[*at];
                held == profile && same_pairs(attributes, pending.pairs[range.clone()].iter())
            })
            .map(|(_, at)| *at)
    }

    fn find_series(
        &self,
        pending: &Pending<'_>,
        resource: usize,
        point: &PendingPoint<'_>,
    ) -> Option<usize> {
        self.series_index
            .find(point.hash, |(_, at)| {
                let series = &self.series[*at];
                series.resource == resource
                    && series.name == point.name
                    && same_attributes(series, &pending.pairs[point.attributes.clone()])
            })
            .map(|(_, at)| *at)
    }

    /// Add a request's points, or none when they do not fit: returns
    /// whether they were added.
    fn commit(&mut self, pending: &Pending<'_>) -> bool {
        let resources: Vec<Option<usize>> = (0..pending.resources.len())
            .map(|document| self.find_resource(pending, document))
            .collect();
        let mut found = Vec::with_capacity(pending.points.len());
        let mut added = 0;
        for (at, point) in pending.points.iter().enumerate() {
            let series =
                resources[point.resource].and_then(|r| self.find_series(pending, r, point));
            found.push(series);
            added += match point.instrument {
                Instrument::Histogram => 1,
                // A Counter series new to the batch, once per request.
                Instrument::Counter if series.is_some() => 0,
                Instrument::Counter => usize::from(!pending.points[..at].iter().any(|earlier| {
                    earlier.instrument == Instrument::Counter && pending.same_series(earlier, point)
                })),
            };
        }
        if let Some(max_len) = self.max_len
            && self.len + added > max_len
        {
            self.dropped += pending.resources.len() as u64;
            return false;
        }
        let mut resources = resources;
        for (point, mut series) in pending.points.iter().zip(found) {
            let resource = match resources[point.resource] {
                Some(resource) => resource,
                None => {
                    let resource = self.insert_resource(pending, point.resource);
                    // Every document with the same resource now finds it.
                    for (document, slot) in resources.iter_mut().enumerate() {
                        if slot.is_none() && self.find_resource(pending, document) == Some(resource)
                        {
                            *slot = Some(resource);
                        }
                    }
                    resource
                }
            };
            if series.is_none() {
                series = self.find_series(pending, resource, point);
            }
            let at = match series {
                Some(at) => at,
                None => self.insert_series(pending, resource, point),
            };
            match &mut self.series[at].data {
                Data::Counter(sum) => *sum = sum.add(point.sum),
                Data::Histogram(samples) => {
                    samples.values.push(point.value);
                    samples.order.push(self.recorded);
                    self.recorded += 1;
                    self.len += 1;
                }
            }
        }
        true
    }

    fn insert_resource(&mut self, pending: &Pending<'_>, document: usize) -> usize {
        let (profile, range, hash) = &pending.resources[document];
        let attributes = pending.pairs[range.clone()]
            .iter()
            .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
            .collect();
        let at = self.resources.len();
        self.resources.push((attributes, *profile));
        self.resource_index
            .insert_unique(*hash, (*hash, at), |(hash, _)| *hash);
        at
    }

    fn insert_series(
        &mut self,
        pending: &Pending<'_>,
        resource: usize,
        point: &PendingPoint<'_>,
    ) -> usize {
        let at = self.series.len();
        let emitted: Vec<(String, String)> = pending.pairs[point.attributes.clone()]
            .iter()
            .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
            .collect();
        self.series.push(Series {
            resource,
            name: point.name.to_string(),
            unit: point.unit.to_string(),
            attributes: emitted.iter().cloned().collect(),
            emitted,
            data: match point.instrument {
                Instrument::Counter => {
                    self.len += 1;
                    Data::Counter(Sum::Integer(0))
                }
                Instrument::Histogram => Data::Histogram(Samples::default()),
            },
        });
        self.series_index
            .insert_unique(point.hash, (point.hash, at), |(hash, _)| *hash);
        at
    }

    pub fn is_empty(&self) -> bool {
        self.series.is_empty()
    }

    /// The number of points the batch holds: one per Counter series and one
    /// per Histogram sample, as [`MetricBatch::points`] returns them. For a
    /// flush threshold; it encodes nothing, and is not a line count: the
    /// DogStatsD lines can be more (each Histogram series also writes a
    /// `.count` and a `.sum` line) or fewer (series of resources whose
    /// Datadog tags normalize alike are counted apart here but share their
    /// lines).
    pub fn len(&self) -> usize {
        self.len
    }

    /// The series grouped by resource and profile, both sorted as the
    /// conformance order sorts them: resource and profile, then name and
    /// attributes.
    pub(crate) fn groups(&self) -> Vec<Group<'_>> {
        let mut order: Vec<&Series> = self.series.iter().collect();
        order.sort_by(|a, b| {
            let (ra, pa) = &self.resources[a.resource];
            let (rb, pb) = &self.resources[b.resource];
            ra.cmp(rb)
                .then(pa.cmp(pb))
                .then_with(|| a.name.cmp(&b.name))
                .then_with(|| a.attributes.cmp(&b.attributes))
        });
        // A resource and profile is held once, so its series are adjacent.
        let mut groups: Vec<(usize, Group<'_>)> = Vec::new();
        for series in order {
            match groups.last_mut() {
                Some((at, group)) if *at == series.resource => group.series.push(series),
                _ => {
                    let (resource, profile) = &self.resources[series.resource];
                    groups.push((
                        series.resource,
                        Group {
                            resource,
                            profile: *profile,
                            series: vec![series],
                        },
                    ));
                }
            }
        }
        groups.into_iter().map(|(_, group)| group).collect()
    }

    /// The batch as points: per resource and profile, one point per Counter
    /// series with its sum, and one point per Histogram sample.
    pub fn points(&self) -> Vec<(Attributes, Profile, Vec<MetricPoint>)> {
        self.groups()
            .into_iter()
            .map(|group| {
                let mut points: Vec<MetricPoint> = group
                    .series
                    .iter()
                    .filter_map(|series| match &series.data {
                        Data::Counter(sum) => Some(counter_point(series, *sum)),
                        Data::Histogram(_) => None,
                    })
                    .collect();
                for series in &group.series {
                    if let Data::Histogram(samples) = &series.data {
                        points.extend(samples.values.iter().map(|value| {
                            MetricPoint::histogram(
                                &series.name,
                                &series.unit,
                                *value,
                                series.attributes.clone(),
                            )
                        }));
                    }
                }
                (group.resource.clone(), group.profile, points)
            })
            .collect()
    }

    /// The Datadog series of the batch, as one submission. Resources whose
    /// tags normalize to the same series share its Counts.
    pub fn datadog_series(&self, historical: bool) -> Result<Vec<DatadogSeries>, MetricError> {
        Ok(self.series_builder(historical, false)?.finish())
    }

    /// The Counts of the batch, and its Distributions with their samples in
    /// recording order, for a take that encodes each Distribution once.
    pub(crate) fn datadog_parts(
        &self,
    ) -> Result<(Vec<DatadogSeries>, Vec<crate::datadog::Distribution>), MetricError> {
        Ok(self.series_builder(false, true)?.finish_parts())
    }

    fn series_builder(
        &self,
        historical: bool,
        keep_samples: bool,
    ) -> Result<SeriesBuilder, MetricError> {
        let mut builder = SeriesBuilder::default();
        builder.keep_samples = keep_samples;
        let groups = self.groups();
        for group in &groups {
            for series in &group.series {
                if let Data::Counter(sum) = &series.data {
                    builder.add(
                        group.profile,
                        std::slice::from_ref(&counter_point(series, *sum)),
                        group.resource,
                        historical,
                    )?;
                }
            }
        }
        // A Histogram's `.sum` adds its samples in the order they were
        // recorded, also when they come from resources whose tags normalize
        // to the same series. Each series is tagged once.
        let mut samples = Vec::new();
        for group in &groups {
            for series in &group.series {
                if let Data::Histogram(held) = &series.data {
                    let distribution =
                        builder.distribution(group.profile, series, group.resource)?;
                    samples.extend(
                        held.order
                            .iter()
                            .zip(&held.values)
                            .map(|(order, value)| (*order, distribution, *value)),
                    );
                }
            }
        }
        samples.sort_unstable_by_key(|(order, ..)| *order);
        for (_, distribution, value) in samples {
            builder.sample(distribution, value, historical);
        }
        Ok(builder)
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

fn counter_point(series: &Series, sum: Sum) -> MetricPoint {
    match sum {
        Sum::Integer(total) => {
            MetricPoint::counter_exact(&series.name, &series.unit, total, series.attributes.clone())
        }
        Sum::Double(total) => {
            MetricPoint::counter(&series.name, &series.unit, total, series.attributes.clone())
        }
    }
}

/// Check a projection of `profile` as [`MetricBatch::add`] does.
fn check_profile(profile: Profile, projection: &Projection) -> Result<(), MetricError> {
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
    Ok(())
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
