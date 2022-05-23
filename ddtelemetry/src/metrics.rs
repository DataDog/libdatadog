// Unless explicitly stated otherwise all files in this repository are licensed under the Apache License Version 2.0.
// This product includes software developed at Datadog (https://www.datadoghq.com/). Copyright 2021-Present Datadog, Inc.

use std::{
    collections::HashMap,
    sync::{Arc, Mutex, MutexGuard},
    time,
};

use ddcommon::tag::Tag;

use crate::data;

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
    fn new(metric_type: data::metrics::MetricType) -> Self {
        Self {
            aggreg: match metric_type {
                data::metrics::MetricType::Count => MetricAggreg::Count { count: 0.0 },
                data::metrics::MetricType::Gauge => MetricAggreg::Gauge { value: 0.0 },
            },
        }
    }

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

#[derive(Debug, PartialEq, Eq, Hash, Clone, Copy)]
pub struct ContextKey(usize);

#[derive(Debug, PartialEq, Eq, Hash)]
struct BucketKey {
    context_key: ContextKey,
    extra_tags: Vec<Tag>,
}

#[derive(Debug, Default)]
pub struct MetricBuckets {
    buckets: HashMap<BucketKey, MetricBucket>,
    series: HashMap<BucketKey, Vec<(u64, f64)>>,
}

impl MetricBuckets {
    pub fn flush_agregates(&mut self) {
        let timestamp = unix_timestamp_now();
        for (key, bucket) in self.buckets.drain() {
            self.series
                .entry(key)
                .or_insert_with(Vec::new)
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

    pub fn add_point(
        &mut self,
        context_key: ContextKey,
        contexts: &MetricContexts,
        point: f64,
        extra_tags: Vec<Tag>,
    ) {
        let bucket_key = BucketKey {
            context_key,
            extra_tags,
        };
        self.buckets
            .entry(bucket_key)
            .or_insert_with(|| {
                let metric_type = contexts.get_metric_type(context_key).unwrap();
                MetricBucket::new(metric_type)
            })
            .add_point(point)
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
    key: ContextKey,
}

impl<'a> MetricContextGuard<'a> {
    pub fn read(&self) -> Option<&MetricContext> {
        self.guard.store.get(self.key.0)
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
        let key = ContextKey(contexts.store.len());
        contexts.store.push(MetricContext {
            name,
            tags,
            metric_type,
            common,
            namespace,
        });
        key
    }

    fn get_metric_type(&self, key: ContextKey) -> Option<data::metrics::MetricType> {
        let guard = self.inner.lock().unwrap();
        // Safe if the Vec is never popped, because the only way to obtain to get a ContextKey is to call register_metric_context
        let MetricContext { metric_type, .. } = guard.store.get(key.0)?;
        Some(*metric_type)
    }

    pub fn get_context(&self, key: ContextKey) -> MetricContextGuard<'_> {
        MetricContextGuard {
            guard: self.inner.as_ref().lock().unwrap(),
            key,
        }
    }
}
