// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use std::{
    collections::HashMap,
    sync::{Arc, Mutex, MutexGuard},
    time,
};

use datadog_ddsketch::DDSketch;
use ddcommon::tag::Tag;
use serde::{Deserialize, Serialize};

use crate::data::{self, metrics};

fn unix_timestamp_now() -> u64 {
    time::SystemTime::UNIX_EPOCH
        .elapsed()
        .map_or(0, |d| d.as_secs())
}

#[derive(Debug)]
struct MetricBucket {
    aggreg: MetricAggreg,
}

#[derive(Debug)]
enum MetricAggreg {
    Count { count: f64 },
    Gauge { value: f64 },
}

impl MetricBucket {
    fn add_point(&mut self, point: f64) {
        match &mut self.aggreg {
            MetricAggreg::Count { count } => *count += point,
            MetricAggreg::Gauge { value } => *value = point,
        }
    }

    fn value(&self) -> f64 {
        match self.aggreg {
            MetricAggreg::Count { count } => count,
            MetricAggreg::Gauge { value } => value,
        }
    }
}

#[derive(Debug, PartialEq, Eq, Hash, Clone, Copy, Serialize, Deserialize)]
#[repr(C)]
pub struct ContextKey(u32, metrics::MetricType);

#[derive(Debug, PartialEq, Eq, Hash)]
struct BucketKey {
    context_key: ContextKey,
    extra_tags: Vec<Tag>,
}

#[derive(Debug, Default)]
pub struct MetricBuckets {
    buckets: HashMap<BucketKey, MetricBucket>,
    series: HashMap<BucketKey, Vec<(u64, f64)>>,
    distributions: HashMap<BucketKey, DDSketch>,
}

#[derive(Default, Serialize, Deserialize)]
pub struct MetricBucketStats {
    pub buckets: u32,
    pub series: u32,
    pub series_points: u32,
    pub distributions: u32,
    pub distributions_points: u32,
}

impl MetricBuckets {
    pub const METRICS_FLUSH_INTERVAL: time::Duration = time::Duration::from_secs(10);

    pub fn flush_agregates(&mut self) {
        let timestamp = unix_timestamp_now();
        for (key, bucket) in self.buckets.drain() {
            self.series
                .entry(key)
                .or_default()
                .push((timestamp, bucket.value()))
        }
    }

    pub fn flush_series(
        &mut self,
    ) -> impl Iterator<Item = (ContextKey, Vec<Tag>, Vec<(u64, f64)>)> + '_ {
        self.series.drain().map(
            |(
                BucketKey {
                    context_key,
                    extra_tags,
                },
                points,
            )| (context_key, extra_tags, points),
        )
    }

    pub fn flush_distributions(
        &mut self,
    ) -> impl Iterator<Item = (ContextKey, Vec<Tag>, DDSketch)> + '_ {
        self.distributions.drain().map(
            |(
                BucketKey {
                    context_key,
                    extra_tags,
                },
                points,
            )| (context_key, extra_tags, points),
        )
    }

    pub fn add_point(&mut self, context_key: ContextKey, point: f64, extra_tags: Vec<Tag>) {
        let bucket_key = BucketKey {
            context_key,
            extra_tags,
        };
        match context_key.1 {
            metrics::MetricType::Count => self
                .buckets
                .entry(bucket_key)
                .or_insert_with(|| MetricBucket {
                    aggreg: MetricAggreg::Count { count: 0.0 },
                })
                .add_point(point),
            metrics::MetricType::Gauge => self
                .buckets
                .entry(bucket_key)
                .or_insert_with(|| MetricBucket {
                    aggreg: MetricAggreg::Gauge { value: 0.0 },
                })
                .add_point(point),
            metrics::MetricType::Distribution => {
                let _ = self.distributions.entry(bucket_key).or_default().add(point);
            }
        }
    }

    pub fn stats(&self) -> MetricBucketStats {
        MetricBucketStats {
            buckets: self.buckets.len() as u32,
            series: self.series.len() as u32,
            series_points: self.series.values().map(|v| v.len() as u32).sum(),
            distributions: self.distributions.len() as u32,
            distributions_points: self
                .distributions
                .values()
                .flat_map(|sketch| {
                    sketch
                        .ordered_bins()
                        .into_iter()
                        .map(|(_, weight)| weight as u32)
                })
                .sum(),
        }
    }
}

#[derive(Debug, Serialize, Deserialize)]
pub struct MetricContext {
    pub namespace: data::metrics::MetricNamespace,
    pub name: String,
    pub tags: Vec<Tag>,
    pub metric_type: data::metrics::MetricType,
    pub common: bool,
}

pub struct MetricContextGuard<'a> {
    guard: MutexGuard<'a, InnerMetricContexts>,
}

impl MetricContextGuard<'_> {
    pub fn read(&self, key: ContextKey) -> Option<&MetricContext> {
        self.guard.store.get(key.0 as usize)
    }

    pub fn is_empty(&self) -> bool {
        self.guard.store.is_empty()
    }

    pub fn len(&self) -> usize {
        self.guard.store.len()
    }
}

#[derive(Debug, Default)]
struct InnerMetricContexts {
    store: Vec<MetricContext>,
}

#[derive(Debug, Clone, Default)]
pub struct MetricContexts {
    inner: Arc<Mutex<InnerMetricContexts>>,
}

impl MetricContexts {
    pub fn register_metric_context(
        &self,
        name: String,
        tags: Vec<Tag>,
        metric_type: data::metrics::MetricType,
        common: bool,
        namespace: data::metrics::MetricNamespace,
    ) -> ContextKey {
        let mut contexts = self.inner.lock().unwrap();
        let key = ContextKey(contexts.store.len() as u32, metric_type);
        contexts.store.push(MetricContext {
            name,
            tags,
            metric_type,
            common,
            namespace,
        });
        key
    }

    pub fn lock(&self) -> MetricContextGuard<'_> {
        MetricContextGuard {
            guard: self.inner.as_ref().lock().unwrap(),
        }
    }
}

#[cfg(test)]
mod tests {
    use ddcommon::tag;
    use std::fmt::Debug;

    use super::*;
    use crate::data::metrics::{MetricNamespace, MetricType};

    /// Check if a and b are approximately equal with the given precision or 1.0e-6 by default
    macro_rules! assert_approx_eq {
        ($a:expr, $b:expr) => {{
            let (a, b) = (&$a, &$b);
            assert!(
                (*a - *b).abs() < 1.0e-6,
                "{} is not approximately equal to {}",
                *a,
                *b
            );
        }};
        ($a:expr, $b:expr, $precision:expr) => {{
            let (a, b) = (&$a, &$b);
            assert!(
                (*a - *b).abs() < $precision,
                "{} is not approximately equal to {}",
                *a,
                *b
            );
        }};
    }

    // Test util used to run assertions against an unsorted list
    fn check_iter<'a, U: 'a + Debug, T: Iterator<Item = &'a U>>(
        elems: T,
        assertions: &[&dyn Fn(&U) -> bool],
    ) {
        let mut used = vec![false; assertions.len()];
        for e in elems {
            let mut found = false;
            for (i, &a) in assertions.iter().enumerate() {
                if a(e) {
                    if used[i] {
                        panic!("Assertion {i} has been used multiple times");
                    }
                    found = true;
                    used[i] = true;
                    break;
                }
            }
            if !found {
                panic!("No assertion found for elem {e:?}")
            }
        }
    }

    #[test]
    fn test_bucket_flushes() {
        let mut buckets = MetricBuckets::default();
        let contexts = MetricContexts::default();

        let context_key_1 = contexts.register_metric_context(
            "metric1".into(),
            Vec::new(),
            MetricType::Gauge,
            false,
            MetricNamespace::Tracers,
        );
        let context_key_2 = contexts.register_metric_context(
            "metric2".into(),
            Vec::new(),
            MetricType::Gauge,
            false,
            MetricNamespace::Tracers,
        );
        let extra_tags = vec![tag!("service", "foobar")];

        buckets.add_point(context_key_1, 0.1, Vec::new());
        buckets.add_point(context_key_1, 0.2, Vec::new());
        assert_eq!(buckets.buckets.len(), 1);

        buckets.add_point(context_key_2, 0.3, Vec::new());
        assert_eq!(buckets.buckets.len(), 2);

        buckets.add_point(context_key_2, 0.4, extra_tags.clone());
        assert_eq!(buckets.buckets.len(), 3);

        buckets.flush_agregates();
        assert_eq!(buckets.buckets.len(), 0);
        assert_eq!(buckets.series.len(), 3);

        buckets.add_point(context_key_1, 0.5, Vec::new());
        buckets.add_point(context_key_2, 0.6, extra_tags);
        assert_eq!(buckets.buckets.len(), 2);

        buckets.flush_agregates();
        assert_eq!(buckets.buckets.len(), 0);
        assert_eq!(buckets.series.len(), 3);

        let series: Vec<_> = buckets.flush_series().collect();
        assert_eq!(buckets.buckets.len(), 0);
        assert_eq!(buckets.series.len(), 0);
        assert_eq!(series.len(), 3);

        check_iter(
            series.iter(),
            &[
                &|(c, t, points)| {
                    if !(c == &context_key_1 && t.is_empty()) {
                        return false;
                    }
                    assert_eq!(points.len(), 2);
                    assert_approx_eq!(points[0].1, 0.2);
                    assert_approx_eq!(points[1].1, 0.5);
                    true
                },
                &|(c, t, points)| {
                    if !(c == &context_key_2 && t.is_empty()) {
                        return false;
                    }
                    assert_eq!(points.len(), 1);
                    assert_approx_eq!(points[0].1, 0.3);
                    true
                },
                &|(c, t, points)| {
                    if !(c == &context_key_2 && !t.is_empty()) {
                        return false;
                    }
                    assert_eq!(points.len(), 2);
                    assert_approx_eq!(points[0].1, 0.4);
                    assert_approx_eq!(points[1].1, 0.6);
                    true
                },
            ],
        );
    }

    #[test]
    fn test_distributions() {
        let mut buckets = MetricBuckets::default();
        let contexts = MetricContexts::default();

        let context_key_distribution = contexts.register_metric_context(
            "metric_distribution".into(),
            Vec::new(),
            MetricType::Distribution,
            false,
            MetricNamespace::Tracers,
        );
        let context_key_distribution_2 = contexts.register_metric_context(
            "metric_distribution_2".into(),
            Vec::new(),
            MetricType::Distribution,
            false,
            MetricNamespace::Tracers,
        );
        let extra_tags = vec![tag!("service", "foo")];

        // Create 2 distributions with 2 and 3 points
        buckets.add_point(context_key_distribution, 1.0, Vec::new());
        buckets.add_point(context_key_distribution, 1.0, Vec::new());
        buckets.add_point(context_key_distribution, 100.0, Vec::new());
        buckets.add_point(context_key_distribution, 1000.0, Vec::new());

        buckets.add_point(context_key_distribution_2, 2.0, Vec::new());
        buckets.add_point(context_key_distribution_2, 200.0, Vec::new());

        buckets.add_point(context_key_distribution_2, 3.0, extra_tags.clone());
        buckets.add_point(context_key_distribution_2, 300.0, extra_tags.clone());

        let distributions: Vec<_> = buckets.flush_distributions().collect();

        check_iter(
            distributions.iter(),
            &[
                &|(c, t, points)| {
                    if !(c == &context_key_distribution && t.is_empty()) {
                        return false;
                    }
                    let bins: Vec<_> = points
                        .ordered_bins()
                        .into_iter()
                        .filter(|(_, w)| *w != 0.0)
                        .collect();
                    assert_eq!(bins.len(), 3);
                    // The precision is quite low since it is up to the ddsketch implementation to
                    // test the precision
                    assert_approx_eq!(bins[0].0, 1.0, 1.0e-1);
                    assert_approx_eq!(bins[0].1, 2.0);
                    assert_approx_eq!(bins[1].0, 100.0, 1.0);
                    assert_approx_eq!(bins[1].1, 1.0);
                    assert_approx_eq!(bins[2].0, 1000.0, 10.0);
                    assert_approx_eq!(bins[2].1, 1.0);
                    true
                },
                &|(c, t, points)| {
                    if !(c == &context_key_distribution_2 && t.is_empty()) {
                        return false;
                    }
                    let bins: Vec<_> = points
                        .ordered_bins()
                        .into_iter()
                        .filter(|(_, w)| *w != 0.0)
                        .collect();
                    assert_eq!(bins.len(), 2);
                    assert_approx_eq!(bins[0].0, 2.0, 1.0e-1);
                    assert_approx_eq!(bins[0].1, 1.0);
                    assert_approx_eq!(bins[1].0, 200.0, 1.0);
                    assert_approx_eq!(bins[1].1, 1.0);
                    true
                },
                &|(c, t, points)| {
                    if !(c == &context_key_distribution_2 && !t.is_empty()) {
                        return false;
                    }
                    let bins: Vec<_> = points
                        .ordered_bins()
                        .into_iter()
                        .filter(|(_, w)| *w != 0.0)
                        .collect();
                    assert_eq!(bins.len(), 2);
                    assert_approx_eq!(bins[0].0, 3.0, 1.0e-1);
                    assert_approx_eq!(bins[0].1, 1.0);
                    assert_approx_eq!(bins[1].0, 300.0, 1.0);
                    assert_approx_eq!(bins[1].1, 1.0);
                    true
                },
            ],
        )
    }

    #[test]
    fn test_stats() {
        let mut buckets = MetricBuckets::default();
        let contexts = MetricContexts::default();

        let context_key_1 = contexts.register_metric_context(
            "metric1".into(),
            Vec::new(),
            MetricType::Count,
            false,
            MetricNamespace::Tracers,
        );

        let context_key_2 = contexts.register_metric_context(
            "metric2".into(),
            Vec::new(),
            MetricType::Gauge,
            false,
            MetricNamespace::Tracers,
        );

        let context_key_distribution = contexts.register_metric_context(
            "metric_distribution".into(),
            Vec::new(),
            MetricType::Distribution,
            false,
            MetricNamespace::Tracers,
        );

        let context_key_distribution_2 = contexts.register_metric_context(
            "metric_distribution_2".into(),
            Vec::new(),
            MetricType::Distribution,
            false,
            MetricNamespace::Tracers,
        );

        // Create 2 series with 2 and 3 points
        buckets.add_point(context_key_1, 1.0, Vec::new());
        buckets.add_point(context_key_2, 2.0, Vec::new());
        buckets.flush_agregates();

        buckets.add_point(context_key_1, 1.0, Vec::new());
        buckets.add_point(context_key_2, 2.0, Vec::new());
        buckets.flush_agregates();

        buckets.add_point(context_key_1, 1.1, Vec::new());
        buckets.add_point(context_key_1, 2.1, Vec::new());
        buckets.flush_agregates();

        // Create 2 buckets
        buckets.add_point(context_key_1, 1.0, Vec::new());
        buckets.add_point(context_key_2, 2.0, Vec::new());

        // Create 2 distributions with 2 and 3 points
        buckets.add_point(context_key_distribution, 1.0, Vec::new());
        buckets.add_point(context_key_distribution, 1.1, Vec::new());
        buckets.add_point(context_key_distribution, 1.2, Vec::new());

        buckets.add_point(context_key_distribution_2, 2.0, Vec::new());
        buckets.add_point(context_key_distribution_2, 2.1, Vec::new());

        let stats = buckets.stats();

        assert_eq!(stats.buckets, 2);
        assert_eq!(stats.series, 2);
        assert_eq!(stats.series_points, 5);
        assert_eq!(stats.distributions, 2);
        assert_eq!(stats.distributions_points, 5);
    }
}
