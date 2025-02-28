// Copyright 2023-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! The aggregation of metrics.

use crate::constants;
use crate::datadog::{self, Metric as MetricToShip, Series};
use crate::errors;
use crate::metric::{self, Metric, MetricValue, SortedTags};

use datadog_protos::metrics::{Dogsketch, Sketch, SketchPayload};
use ddsketch_agent::DDSketch;
use hashbrown::hash_table;
use protobuf::Message;
use tracing::{error, warn};
use ustr::Ustr;

impl MetricValue {
    fn aggregate(&mut self, metric: Metric) {
        // safe because we know there's at least one value when we parse
        // TODO aggregating different types should return error
        match self {
            MetricValue::Count(count) => *count += metric.value.get_value().unwrap_or_default(),
            MetricValue::Gauge(gauge) => *gauge = metric.value.get_value().unwrap_or_default(),
            MetricValue::Distribution(distribution) => {
                if let Some(value) = metric.value.get_sketch() {
                    distribution.merge(value);
                }
            }
        }
    }

    pub fn get_value(&self) -> Option<f64> {
        match self {
            MetricValue::Count(count) => Some(*count),
            MetricValue::Gauge(gauge) => Some(*gauge),
            MetricValue::Distribution(_) => None,
        }
    }

    pub fn get_sketch(&self) -> Option<&DDSketch> {
        match self {
            MetricValue::Distribution(distribution) => Some(distribution),
            _ => None,
        }
    }
}

#[derive(Clone)]
// NOTE by construction we know that intervals and contexts do not explore the
// full space of usize but the type system limits how we can express this today.
pub struct Aggregator {
    tags: SortedTags,
    map: hash_table::HashTable<Metric>,
    max_batch_entries_single_metric: usize,
    max_batch_bytes_single_metric: u64,
    max_batch_entries_sketch_metric: usize,
    max_batch_bytes_sketch_metric: u64,
    max_context: usize,
}

impl Aggregator {
    /// Create a new instance of `Aggregator`
    ///
    /// # Errors
    ///
    /// Will fail at runtime if the type `INTERVALS` and `CONTEXTS` exceed their
    /// counterparts in `constants`. This would be better as a compile-time
    /// issue, although leaving this open allows for runtime configuration.
    #[allow(clippy::cast_precision_loss)]
    pub fn new(tags: SortedTags, max_context: usize) -> Result<Self, errors::Creation> {
        if max_context > constants::MAX_CONTEXTS {
            return Err(errors::Creation::Contexts);
        }
        Ok(Self {
            tags,
            map: hash_table::HashTable::new(),
            max_batch_entries_single_metric: constants::MAX_ENTRIES_SINGLE_METRIC,
            max_batch_bytes_single_metric: constants::MAX_SIZE_BYTES_SINGLE_METRIC,
            max_batch_entries_sketch_metric: constants::MAX_ENTRIES_SKETCH_METRIC,
            max_batch_bytes_sketch_metric: constants::MAX_SIZE_SKETCH_METRIC,
            max_context,
        })
    }

    /// Insert a `Metric` into the `Aggregator` at the current interval
    ///
    /// # Errors
    ///
    /// Function will return overflow error if more than
    /// `min(constants::MAX_CONTEXTS, CONTEXTS)` is exceeded.
    pub fn insert(&mut self, metric: Metric) -> Result<(), errors::Insert> {
        let id = metric::id(metric.name, &metric.tags, metric.timestamp);
        let len = self.map.len();

        match self.map.entry(
            id,
            |m| m.id == id,
            |m| metric::id(m.name, &m.tags, m.timestamp),
        ) {
            hash_table::Entry::Vacant(entry) => {
                if len >= self.max_context {
                    return Err(errors::Insert::Overflow);
                }
                entry.insert(metric);
            }
            hash_table::Entry::Occupied(mut entry) => {
                entry.get_mut().value.aggregate(metric);
            }
        }
        Ok(())
    }

    pub fn clear(&mut self) {
        self.map.clear();
    }

    #[must_use]
    pub fn distributions_to_protobuf(&self) -> SketchPayload {
        let mut sketch_payload = SketchPayload::new();

        sketch_payload.sketches = self
            .map
            .iter()
            .filter_map(|entry| match entry.value {
                MetricValue::Distribution(_) => build_sketch(entry, self.tags.clone()),
                _ => None,
            })
            .collect();
        sketch_payload
    }

    #[must_use]
    pub fn consume_distributions(&mut self) -> Vec<SketchPayload> {
        let mut batched_payloads = Vec::new();
        let mut sketch_payload = SketchPayload::new();
        let mut this_batch_size = 0u64;
        for sketch in self
            .map
            .extract_if(|entry| {
                if let MetricValue::Distribution(_) = entry.value {
                    return true;
                }
                false
            })
            .filter_map(|entry| build_sketch(&entry, self.tags.clone()))
        {
            let next_chunk_size = sketch.compute_size();

            if (sketch_payload.sketches.len() >= self.max_batch_entries_sketch_metric)
                || (this_batch_size + next_chunk_size >= self.max_batch_bytes_sketch_metric)
            {
                if this_batch_size == 0 {
                    warn!("Only one distribution exceeds max batch size, adding it anyway: {:?} with {}", sketch.metric, next_chunk_size);
                } else {
                    batched_payloads.push(sketch_payload);
                    sketch_payload = SketchPayload::new();
                    this_batch_size = 0u64;
                }
            }
            this_batch_size += next_chunk_size;
            sketch_payload.sketches.push(sketch);
        }
        if !sketch_payload.sketches.is_empty() {
            batched_payloads.push(sketch_payload);
        }
        batched_payloads
    }

    #[must_use]
    pub fn to_series(&self) -> Series {
        let mut series_payload = Series {
            series: Vec::with_capacity(1_024),
        };

        self.map
            .iter()
            .filter_map(|entry| match entry.value {
                MetricValue::Distribution(_) => None,
                _ => build_metric(entry, self.tags.clone()),
            })
            .for_each(|metric| series_payload.series.push(metric));
        series_payload
    }

    #[must_use]
    pub fn consume_metrics(&mut self) -> Vec<Series> {
        let mut batched_payloads = Vec::new();
        let mut series_payload = Series {
            series: Vec::with_capacity(1_024),
        };
        let mut this_batch_size = 0u64;
        for metric in self
            .map
            .extract_if(|entry| {
                if let MetricValue::Distribution(_) = entry.value {
                    return false;
                }
                true
            })
            .filter_map(|entry| build_metric(&entry, self.tags.clone()))
        {
            // TODO serialization is made twice for each point. If we return a Vec<u8> we can avoid
            // that
            let serialized_metric_size = match serde_json::to_vec(&metric) {
                Ok(serialized_metric) => serialized_metric.len() as u64,
                Err(e) => {
                    error!("failed to serialize metric: {:?}", e);
                    0u64
                }
            };

            if serialized_metric_size > 0 {
                if (series_payload.series.len() >= self.max_batch_entries_single_metric)
                    || (this_batch_size + serialized_metric_size
                        >= self.max_batch_bytes_single_metric)
                {
                    if this_batch_size == 0 {
                        warn!("Only one metric exceeds max batch size, adding it anyway: {:?} with {}", metric.metric, serialized_metric_size);
                    } else {
                        batched_payloads.push(series_payload);
                        series_payload = Series {
                            series: Vec::with_capacity(1_024),
                        };
                        this_batch_size = 0u64;
                    }
                }
                series_payload.series.push(metric);
                this_batch_size += serialized_metric_size;
            }
        }

        if !series_payload.series.is_empty() {
            batched_payloads.push(series_payload);
        }
        batched_payloads
    }

    pub fn get_entry_by_id(
        &self,
        name: Ustr,
        tags: &Option<SortedTags>,
        timestamp: i64,
    ) -> Option<&Metric> {
        let id = metric::id(name, tags, timestamp);
        self.map.find(id, |m| m.id == id)
    }
}

fn build_sketch(entry: &Metric, mut base_tag_vec: SortedTags) -> Option<Sketch> {
    let sketch = entry.value.get_sketch()?;
    let mut dogsketch = Dogsketch::default();
    sketch.merge_to_dogsketch(&mut dogsketch);
    // TODO(Astuyve) allow users to specify timestamp
    dogsketch.set_ts(entry.timestamp);
    let mut sketch = Sketch::default();
    sketch.set_dogsketches(vec![dogsketch]);
    let name = entry.name.to_string();
    sketch.set_metric(name.clone().into());
    if let Some(tags) = entry.tags.clone() {
        base_tag_vec.extend(&tags);
    }
    sketch.set_tags(base_tag_vec.to_chars());
    Some(sketch)
}

fn build_metric(entry: &Metric, mut base_tag_vec: SortedTags) -> Option<MetricToShip> {
    let resources;
    if let Some(tags) = entry.tags.clone() {
        resources = tags.to_resources();
    } else {
        resources = Vec::new();
    }
    let kind = match entry.value {
        MetricValue::Count(_) => datadog::DdMetricKind::Count,
        MetricValue::Gauge(_) => datadog::DdMetricKind::Gauge,
        MetricValue::Distribution(_) => unreachable!(),
    };
    let point = datadog::Point {
        value: entry.value.get_value()?,
        // TODO(astuyve) allow user to specify timestamp
        timestamp: entry.timestamp as u64,
    };

    if let Some(tags) = entry.tags.clone() {
        base_tag_vec.extend(&tags);
    }

    Some(MetricToShip {
        metric: entry.name.as_str(),
        resources,
        kind,
        points: [point; 1],
        tags: base_tag_vec.to_strings(),
    })
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
pub mod tests {
    use crate::aggregator::Aggregator;
    use crate::metric;
    use crate::metric::{parse, SortedTags, EMPTY_TAGS};
    use datadog_protos::metrics::SketchPayload;
    use hashbrown::hash_table;
    use protobuf::Message;
    use std::sync::Mutex;

    const PRECISION: f64 = 0.000_000_01;

    const SINGLE_METRIC_SIZE: usize = 193; // taken from the test, size of a serialized metric with one tag and 1 digit counter value
    const SINGLE_DISTRIBUTION_SIZE: u64 = 140;
    const DEFAULT_TAGS: &str =
        "dd_extension_version:63-next,architecture:x86_64,_dd.compute_stats:1";

    pub fn assert_value(
        aggregator_mutex: &Mutex<Aggregator>,
        metric_id: &str,
        value: f64,
        tags: &str,
        timestamp: i64,
    ) {
        let aggregator = aggregator_mutex.lock().unwrap();
        if let Some(e) = aggregator.get_entry_by_id(
            metric_id.into(),
            &Some(SortedTags::parse(tags).unwrap()),
            timestamp,
        ) {
            let metric = e.value.get_value().unwrap();
            assert!((metric - value).abs() < PRECISION);
        } else {
            panic!("{}", format!("{metric_id} not found"));
        }
    }

    pub fn assert_sketch(
        aggregator_mutex: &Mutex<Aggregator>,
        metric_id: &str,
        value: f64,
        timestamp: i64,
    ) {
        let aggregator = aggregator_mutex.lock().unwrap();
        if let Some(e) = aggregator.get_entry_by_id(metric_id.into(), &None, timestamp) {
            let metric = e.value.get_sketch().unwrap();
            assert!((metric.max().unwrap() - value).abs() < PRECISION);
            assert!((metric.min().unwrap() - value).abs() < PRECISION);
            assert!((metric.sum().unwrap() - value).abs() < PRECISION);
            assert!((metric.avg().unwrap() - value).abs() < PRECISION);
        } else {
            panic!("{}", format!("{metric_id} not found"));
        }
    }

    #[test]
    #[cfg_attr(miri, ignore)]
    fn insertion() {
        let mut aggregator = Aggregator::new(EMPTY_TAGS, 2).unwrap();

        let metric1 = parse("test:1|c|#k:v").expect("metric parse failed");
        let metric2 = parse("foo:1|c|#k:v").expect("metric parse failed");

        assert!(aggregator.insert(metric1).is_ok());
        assert!(aggregator.insert(metric2).is_ok());

        // Both unique contexts get one slot.
        assert_eq!(aggregator.map.len(), 2);
    }

    #[test]
    #[cfg_attr(miri, ignore)]
    fn distribution_insertion() {
        let mut aggregator = Aggregator::new(EMPTY_TAGS, 2).unwrap();

        let metric1 = parse("test:1|d|#k:v").expect("metric parse failed");
        let metric2 = parse("foo:1|d|#k:v").expect("metric parse failed");

        assert!(aggregator.insert(metric1).is_ok());
        assert!(aggregator.insert(metric2).is_ok());

        // Both unique contexts get one slot.
        assert_eq!(aggregator.map.len(), 2);
    }

    #[test]
    #[cfg_attr(miri, ignore)]
    fn overflow() {
        let mut aggregator = Aggregator::new(EMPTY_TAGS, 2).unwrap();
        let mut now = std::time::UNIX_EPOCH
            .elapsed()
            .expect("unable to poll clock, unrecoverable")
            .as_secs()
            .try_into()
            .unwrap_or_default();
        now = (now / 10) * 10;
        let metric1 = parse("test:1|c|#k:v").expect("metric parse failed");
        let metric2 = parse("foo:1|c|#k:v").expect("metric parse failed");
        let metric3 = parse("bar:1|c|#k:v").expect("metric parse failed");

        let id1 = metric::id(metric1.name, &metric1.tags, now);
        let id2 = metric::id(metric2.name, &metric2.tags, now);
        let id3 = metric::id(metric3.name, &metric3.tags, now);

        assert_ne!(id1, id2);
        assert_ne!(id1, id3);
        assert_ne!(id2, id3);

        assert!(aggregator.insert(metric1).is_ok());
        assert_eq!(aggregator.map.len(), 1);

        assert!(aggregator.insert(metric2.clone()).is_ok());
        assert!(aggregator.insert(metric2.clone()).is_ok());
        assert!(aggregator.insert(metric2).is_ok());
        assert_eq!(aggregator.map.len(), 2);

        assert!(aggregator.insert(metric3).is_err());
        assert_eq!(aggregator.map.len(), 2);
    }

    #[test]
    #[allow(clippy::float_cmp)]
    #[cfg_attr(miri, ignore)]
    fn clear() {
        let mut aggregator = Aggregator::new(EMPTY_TAGS, 2).unwrap();
        let mut now = 1656581409;
        now = (now / 10) * 10;
        let metric1 = parse("test:3|c|#k1:v1|T1656581409").expect("metric parse failed");
        let metric2 = parse("foo:5|c|#k2:v2|T1656581409").expect("metric parse failed");

        assert!(aggregator.insert(metric1).is_ok());
        assert!(aggregator.insert(metric2).is_ok());

        assert_eq!(aggregator.map.len(), 2);
        if let Some(v) = aggregator.get_entry_by_id(
            "foo".into(),
            &Some(SortedTags::parse("k2:v2").unwrap()),
            now,
        ) {
            assert_eq!(v.value.get_value().unwrap(), 5f64);
        } else {
            panic!("failed to get value by id");
        }

        if let Some(v) = aggregator.get_entry_by_id(
            "test".into(),
            &Some(SortedTags::parse("k1:v1").unwrap()),
            now,
        ) {
            assert_eq!(v.value.get_value().unwrap(), 3f64);
        } else {
            panic!("failed to get value by id");
        }

        aggregator.clear();
        assert_eq!(aggregator.map.len(), 0);
    }

    #[test]
    #[cfg_attr(miri, ignore)]
    fn to_series() {
        let mut aggregator = Aggregator::new(EMPTY_TAGS, 2).unwrap();

        let metric1 = parse("test:1|c|#k1:v1,k2:v2").expect("metric parse failed");
        let metric2 = parse("foo:1|c|#k:v").expect("metric parse failed");
        let metric3 = parse("bar:1|c|#k:v").expect("metric parse failed");

        assert!(aggregator.insert(metric1).is_ok());
        assert!(aggregator.insert(metric2).is_ok());

        assert_eq!(aggregator.map.len(), 2);
        assert_eq!(aggregator.to_series().len(), 2);
        // to_series should not mutate the state
        assert_eq!(aggregator.map.len(), 2);
        assert_eq!(aggregator.to_series().len(), 2);
        assert_eq!(aggregator.map.len(), 2);

        assert!(aggregator.insert(metric3).is_err());
        assert_eq!(aggregator.to_series().len(), 2);
    }

    #[test]
    #[cfg_attr(miri, ignore)]
    fn distributions_to_protobuf() {
        let mut aggregator = Aggregator::new(EMPTY_TAGS, 2).unwrap();

        let metric1 = parse("test:1|d|#k:v").expect("metric parse failed");
        let metric2 = parse("foo:1|d|#k:v").expect("metric parse failed");

        assert!(aggregator.insert(metric1).is_ok());
        assert!(aggregator.insert(metric2).is_ok());

        assert_eq!(aggregator.map.len(), 2);
        assert_eq!(aggregator.distributions_to_protobuf().sketches().len(), 2);
        assert_eq!(aggregator.map.len(), 2);
        assert_eq!(aggregator.distributions_to_protobuf().sketches().len(), 2);
        assert_eq!(aggregator.map.len(), 2);
    }

    #[test]
    #[cfg_attr(miri, ignore)]
    fn consume_distributions_ignore_single_metrics() {
        let mut aggregator = Aggregator::new(EMPTY_TAGS, 1_000).unwrap();
        assert_eq!(aggregator.distributions_to_protobuf().sketches.len(), 0);

        assert!(aggregator
            .insert(parse("test1:1|d|#k:v".to_string().as_str()).expect("metric parse failed"))
            .is_ok());
        assert_eq!(aggregator.distributions_to_protobuf().sketches.len(), 1);

        assert!(aggregator
            .insert(parse("foo:1|c|#k:v").expect("metric parse failed"))
            .is_ok());
        assert_eq!(aggregator.distributions_to_protobuf().sketches.len(), 1);
    }

    #[test]
    #[cfg_attr(miri, ignore)]
    fn consume_distributions_batch_entries() {
        let max_batch = 5;
        let tot = 12;
        let mut aggregator = Aggregator {
            tags: EMPTY_TAGS,
            map: hash_table::HashTable::new(),
            max_batch_entries_single_metric: 1_000,
            max_batch_bytes_single_metric: 1_000,
            max_batch_entries_sketch_metric: max_batch,
            max_batch_bytes_sketch_metric: 1_500,
            max_context: 1_000,
        };

        add_metrics(tot, &mut aggregator, "d".to_string());
        let batched = aggregator.consume_distributions();
        assert_eq!(aggregator.consume_distributions().len(), 0);

        assert_eq!(batched.len(), 3);
        assert_eq!(batched.first().unwrap().sketches.len(), max_batch);
        assert_eq!(batched.get(1).unwrap().sketches.len(), max_batch);
        assert_eq!(batched.get(2).unwrap().sketches.len(), tot - max_batch * 2);
    }

    #[test]
    #[cfg_attr(miri, ignore)]
    fn consume_distributions_batch_bytes() {
        let expected_distribution_per_batch = 2;
        let total_number_of_distributions = 5;
        let max_bytes = SINGLE_DISTRIBUTION_SIZE * expected_distribution_per_batch as u64;
        let mut aggregator = Aggregator {
            tags: to_sorted_tags(),
            map: hash_table::HashTable::new(),
            max_batch_entries_single_metric: 1_000,
            max_batch_bytes_single_metric: 1_000,
            max_batch_entries_sketch_metric: 1_000,
            max_batch_bytes_sketch_metric: max_bytes,
            max_context: 1_000,
        };

        add_metrics(
            total_number_of_distributions,
            &mut aggregator,
            "d".to_string(),
        );
        let batched = aggregator.consume_distributions();

        assert_eq!(
            batched.len(),
            total_number_of_distributions / expected_distribution_per_batch + 1
        );
        assert_eq!(
            batched.first().unwrap().compute_size(),
            SINGLE_DISTRIBUTION_SIZE * expected_distribution_per_batch as u64
        );
        assert_eq!(
            batched.get(1).unwrap().compute_size(),
            SINGLE_DISTRIBUTION_SIZE * expected_distribution_per_batch as u64
        );
        assert_eq!(
            batched.get(2).unwrap().compute_size(),
            SINGLE_DISTRIBUTION_SIZE
        );
    }

    fn to_sorted_tags() -> SortedTags {
        SortedTags::parse(DEFAULT_TAGS).unwrap()
    }

    #[test]
    #[cfg_attr(miri, ignore)]
    fn consume_distribution_one_element_bigger_than_max_size() {
        let max_bytes = 1;
        let tot = 5;
        let mut aggregator = Aggregator {
            tags: to_sorted_tags(),
            map: hash_table::HashTable::new(),
            max_batch_entries_single_metric: 1_000,
            max_batch_bytes_single_metric: 1_000,
            max_batch_entries_sketch_metric: 1_000,
            max_batch_bytes_sketch_metric: max_bytes,
            max_context: 1_000,
        };

        add_metrics(tot, &mut aggregator, "d".to_string());
        let batched = aggregator.consume_distributions();

        assert_eq!(batched.len(), tot);
        for a_batch in batched {
            assert_eq!(a_batch.compute_size(), SINGLE_DISTRIBUTION_SIZE);
        }
    }

    fn add_metrics(tot: usize, aggregator: &mut Aggregator, counter_or_distro: String) {
        for i in 1..=tot {
            assert!(aggregator
                .insert(
                    parse(format!("test{i}:{i}|{counter_or_distro}|#k:v").as_str())
                        .expect("metric parse failed")
                )
                .is_ok());
        }
    }

    #[test]
    #[cfg_attr(miri, ignore)]
    fn consume_series_ignore_distribution() {
        let mut aggregator = Aggregator::new(EMPTY_TAGS, 1_000).unwrap();

        assert_eq!(aggregator.consume_metrics().len(), 0);

        assert!(aggregator
            .insert(parse("test1:1|c|#k:v".to_string().as_str()).expect("metric parse failed"))
            .is_ok());
        assert_eq!(aggregator.consume_distributions().len(), 0);
        assert_eq!(aggregator.consume_metrics().len(), 1);
        assert_eq!(aggregator.consume_metrics().len(), 0);

        assert!(aggregator
            .insert(parse("test1:1|c|#k:v".to_string().as_str()).expect("metric parse failed"))
            .is_ok());
        assert!(aggregator
            .insert(parse("foo:1|d|#k:v").expect("metric parse failed"))
            .is_ok());
        assert_eq!(aggregator.consume_metrics().len(), 1);
        assert_eq!(aggregator.consume_distributions().len(), 1);
        assert_eq!(aggregator.consume_distributions().len(), 0);
    }

    #[test]
    #[cfg_attr(miri, ignore)]
    fn consume_series_batch_entries() {
        let max_batch = 5;
        let tot = 13;
        let mut aggregator = Aggregator {
            tags: EMPTY_TAGS,
            map: hash_table::HashTable::new(),
            max_batch_entries_single_metric: max_batch,
            max_batch_bytes_single_metric: 10_000,
            max_batch_entries_sketch_metric: 1_000,
            max_batch_bytes_sketch_metric: 1_500,
            max_context: 1_000,
        };

        add_metrics(tot, &mut aggregator, "c".to_string());

        let batched = aggregator.consume_metrics();
        assert_eq!(batched.len(), 3);
        assert_eq!(batched.first().unwrap().series.len(), max_batch);
        assert_eq!(batched.get(1).unwrap().series.len(), max_batch);
        assert_eq!(batched.get(2).unwrap().series.len(), tot - max_batch * 2);

        assert_eq!(aggregator.consume_metrics().len(), 0);
    }

    #[test]
    #[cfg_attr(miri, ignore)]
    fn consume_metrics_batch_bytes() {
        let expected_metrics_per_batch = 2;
        let total_number_of_metrics = 5;
        let two_metrics_size = 374;
        let max_bytes = SINGLE_METRIC_SIZE * expected_metrics_per_batch + 13;
        let mut aggregator = Aggregator {
            tags: to_sorted_tags(),
            map: hash_table::HashTable::new(),
            max_batch_entries_single_metric: 1_000,
            max_batch_bytes_single_metric: max_bytes as u64,
            max_batch_entries_sketch_metric: 1_000,
            max_batch_bytes_sketch_metric: 1_000,
            max_context: 1_000,
        };

        add_metrics(total_number_of_metrics, &mut aggregator, "c".to_string());
        let batched = aggregator.consume_metrics();

        assert_eq!(
            batched.len(),
            total_number_of_metrics / expected_metrics_per_batch + 1
        );
        assert_eq!(
            serde_json::to_vec(batched.first().unwrap()).unwrap().len(),
            two_metrics_size
        );
        assert_eq!(
            serde_json::to_vec(batched.get(1).unwrap()).unwrap().len(),
            two_metrics_size
        );
        assert_eq!(
            serde_json::to_vec(batched.get(2).unwrap()).unwrap().len(),
            SINGLE_METRIC_SIZE
        );
    }

    #[test]
    #[cfg_attr(miri, ignore)]
    fn consume_series_one_element_bigger_than_max_size() {
        let max_bytes = 1;
        let tot = 5;
        let mut aggregator = Aggregator {
            tags: to_sorted_tags(),
            map: hash_table::HashTable::new(),
            max_batch_entries_single_metric: 1_000,
            max_batch_bytes_single_metric: max_bytes,
            max_batch_entries_sketch_metric: 1_000,
            max_batch_bytes_sketch_metric: 1_000,
            max_context: 1_000,
        };

        add_metrics(tot, &mut aggregator, "c".to_string());
        let batched = aggregator.consume_metrics();

        assert_eq!(batched.len(), tot);
        for a_batch in batched {
            assert_eq!(
                serde_json::to_vec(&a_batch).unwrap().len(),
                SINGLE_METRIC_SIZE
            );
        }
    }

    #[test]
    #[cfg_attr(miri, ignore)]
    fn distribution_serialized_deserialized() {
        let mut aggregator = Aggregator::new(EMPTY_TAGS, 1_000).unwrap();

        add_metrics(10, &mut aggregator, "d".to_string());
        let distribution = aggregator.distributions_to_protobuf();
        assert_eq!(distribution.sketches().len(), 10);

        let serialized = distribution
            .write_to_bytes()
            .expect("Can't serialized proto");

        let deserialized =
            SketchPayload::parse_from_bytes(serialized.as_slice()).expect("failed to parse proto");

        assert_eq!(deserialized.sketches().len(), 10);
        assert_eq!(deserialized, distribution);
    }
}
