// Unless explicitly stated otherwise all files in this repository are licensed under the Apache License Version 2.0.
// This product includes software developed at Datadog (https://www.datadoghq.com/). Copyright 2021-Present Datadog, Inc.

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
    time::SystemTime::now()
        .duration_since(time::SystemTime::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
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
pub struct ContextKey(u32, metrics::MetricType);

#[repr(u8)]
#[derive(Debug, PartialEq, Eq, Hash, Clone, Copy, Serialize, Deserialize)]
enum MetricType {
    Count,
    Gauge,
    Sketches,
}

#[derive(Debug, PartialEq, Eq, Hash)]
struct BucketKey {
    context_key: ContextKey,
    extra_tags: Vec<Tag>,
}

#[derive(Debug, Default)]
pub struct MetricBuckets {
    buckets: HashMap<BucketKey, MetricBucket>,
    series: HashMap<BucketKey, Vec<(u64, f64)>>,
    sketches: HashMap<BucketKey, DDSketch>,
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

    pub fn flush_sketches(
        &mut self,
    ) -> impl Iterator<Item = (ContextKey, Vec<Tag>, DDSketch)> + '_ {
        self.sketches.drain().map(
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
                self.sketches.entry(bucket_key).or_default().add(point);
            }
        }
    }
}

#[derive(Debug)]
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

impl<'a> MetricContextGuard<'a> {
    pub fn read(&self, key: ContextKey) -> Option<&MetricContext> {
        self.guard.store.get(key.0 as usize)
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
mod test {
    use std::fmt::Debug;

    use super::*;
    use crate::data::metrics::{MetricNamespace, MetricType};

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
    }

    // Test util used to run assertions against an unsorted list
    fn check_iter<'a, U: 'a + Debug, T: Iterator<Item = &'a U>>(
        elems: T,
        assertions: &[&dyn Fn(&U) -> bool],
    ) {
        let used = vec![false; assertions.len()];
        for e in elems {
            let mut found = false;
            for (i, &a) in assertions.iter().enumerate() {
                if a(e) {
                    if used[i] {
                        panic!("Assertion {i} has been used multiple times");
                    }
                    found = true;
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
        let extra_tags = vec![Tag::from_value("service:foobar").unwrap()];

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
}
