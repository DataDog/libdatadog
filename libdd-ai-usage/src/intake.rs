// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! A batch as Datadog metrics intake takes it from a writer that submits
//! straight to the API (a gateway, not through the Agent): Counters as RATE
//! points over the flush interval, distributions as one DDSketch per series
//! in intake's `sketchpb.Sparse` form, stamped at a second no earlier flush
//! of the same writer used, and tagged with the writer's identity.
//!
//! Metrics intake keeps one write per metric, tag set and timestamp, so two
//! writers must never share a series (`host.name` and `process.pid` keep
//! them apart) and one writer must never stamp a series twice at the same
//! second.

use std::collections::BTreeMap;

use libdd_ddsketch::DDSketch;

use crate::MetricBatch;
use crate::datadog::{DatadogSeries, SeriesType, datadog_tag_value};
use crate::point::MetricError;

/// `ε` of intake's DDSketch: `γ = 1 + 2ε`, a relative accuracy slightly
/// better than 1%.
const EPSILON: f64 = 1.0 / 128.0;
/// The smallest value intake guarantees is not rounded to zero.
const MIN_EXPECTED: f64 = 1e-9;
/// The key of `+∞`, and the largest finite key below it.
const INF_KEY: i32 = (1 << 15) - 1;
const MAX_KEY: i32 = INF_KEY - 1;

fn gamma() -> f64 {
    1.0 + 2.0 * EPSILON
}

fn log_gamma(x: f64) -> f64 {
    x.ln() / (2.0 * EPSILON).ln_1p()
}

/// The key offset that makes the bin of [`MIN_EXPECTED`] key 1.
fn key_bias() -> i32 {
    -(log_gamma(MIN_EXPECTED).floor() as i32) + 1
}

/// The smallest value that gets a key of its own; smaller values are key 0.
/// Computed as the Agent and intake compute it, which keeps them alike.
fn min_supported() -> f64 {
    gamma().powf(f64::from(1 - key_bias()))
}

/// The sketch key of `v`: `i` with `γ^(i-0.5) <= v < γ^(i+0.5)`, as intake
/// and the Agent compute it (round half to even).
fn sketch_key(v: f64) -> i32 {
    if v < 0.0 {
        return -sketch_key(-v);
    }
    if v < min_supported() {
        return 0;
    }
    if v.is_infinite() {
        return INF_KEY;
    }
    let i = log_gamma(v).round_ties_even() as i64 + i64::from(key_bias());
    if i > i64::from(MAX_KEY) {
        INF_KEY
    } else if i < 1 {
        1
    } else {
        i as i32
    }
}

/// A distribution's samples as one sketch in intake's `sketchpb.Sparse`
/// form: sorted keys with their counts, and the samples' min, max, count and
/// sum (in recording order).
#[derive(Debug, Clone, PartialEq)]
pub struct Sketch {
    pub keys: Vec<i32>,
    pub counts: Vec<u32>,
    pub min: f64,
    pub max: f64,
    pub count: u64,
    pub sum: f64,
}

impl Sketch {
    /// The sketch of `samples`; NaNs are left out. `None` when none is left.
    pub fn of(samples: &[f64]) -> Option<Sketch> {
        // libdatadog's DDSketch holds the bins: intake's keys, its logarithmic
        // mapping (gamma `1 + 2/128`, offset the key bias), and its store,
        // which folds the lowest bins together past its size.
        let mut sketch = DDSketch::with_log_mapping(gamma(), f64::from(key_bias()))?;
        let (mut min, mut max, mut sum, mut count) = (f64::INFINITY, f64::NEG_INFINITY, 0.0, 0u64);
        for &v in samples.iter().filter(|v| !v.is_nan()) {
            // Usage values are never negative; one would be counted as zero.
            match sketch_key(v) {
                key if key > 0 => sketch.add_to_bin(key, 1.0).ok()?,
                _ => sketch.add(0.0).ok()?,
            }
            min = min.min(v);
            max = max.max(v);
            sum += v;
            count += 1;
        }
        if count == 0 {
            return None;
        }
        // A bin past u32 is written as several bins with the same key.
        let (mut keys, mut counts) = (Vec::new(), Vec::new());
        for (value, weight) in sketch.ordered_bins() {
            let mut n = weight as u64;
            if n == 0 {
                continue;
            }
            let key = if value == 0.0 {
                0
            } else {
                sketch.bin_index(value)?
            };
            while n > u64::from(u32::MAX) {
                keys.push(key);
                counts.push(u32::MAX);
                n -= u64::from(u32::MAX);
            }
            keys.push(key);
            counts.push(n as u32);
        }
        Some(Sketch {
            keys,
            counts,
            min,
            max,
            count,
            sum,
        })
    }

    /// The sketch as protobuf bytes of intake's `sketchpb.Sparse`
    /// (`k = 1` packed `sint32`, `n = 2` packed `uint32`, `basic = 3` with
    /// `sum = 1`, `count = 3`, `min = 4`, `max = 5`).
    pub fn encode_sparse(&self) -> Vec<u8> {
        fn varint(out: &mut Vec<u8>, mut v: u64) {
            while v >= 0x80 {
                out.push((v as u8) | 0x80);
                v >>= 7;
            }
            out.push(v as u8);
        }
        fn packed(out: &mut Vec<u8>, field: u64, values: impl Iterator<Item = u64>) {
            let mut body = Vec::new();
            for v in values {
                varint(&mut body, v);
            }
            if body.is_empty() {
                return;
            }
            varint(out, (field << 3) | 2);
            varint(out, body.len() as u64);
            out.extend_from_slice(&body);
        }
        fn double(out: &mut Vec<u8>, field: u64, v: f64) {
            if v != 0.0 || v.is_sign_negative() {
                varint(out, (field << 3) | 1);
                out.extend_from_slice(&v.to_le_bytes());
            }
        }
        let mut out = Vec::with_capacity(16 + self.keys.len() * 4);
        packed(
            &mut out,
            1,
            self.keys
                .iter()
                .map(|&k| u64::from(((k << 1) ^ (k >> 31)) as u32)),
        );
        packed(&mut out, 2, self.counts.iter().map(|&n| u64::from(n)));
        let mut basic = Vec::with_capacity(36);
        double(&mut basic, 1, self.sum);
        if self.count != 0 {
            varint(&mut basic, 3 << 3);
            varint(&mut basic, self.count);
        }
        double(&mut basic, 4, self.min);
        double(&mut basic, 5, self.max);
        varint(&mut out, (3 << 3) | 2);
        varint(&mut out, basic.len() as u64);
        out.extend_from_slice(&basic);
        out
    }
}

/// One Counter series as a RATE point: the sum since the last take divided
/// by the interval, to be aggregated by SUM across writers.
#[derive(Debug, Clone, PartialEq)]
pub struct RateSeries {
    pub metric: String,
    pub tags: Vec<String>,
    pub value: f64,
}

/// One distribution series as a sketch of its samples since the last take.
#[derive(Debug, Clone, PartialEq)]
pub struct SketchSeries {
    pub metric: String,
    pub tags: Vec<String>,
    pub sketch: Sketch,
}

/// What [`MetricBatch::take_intake`] took.
#[derive(Debug, Clone, PartialEq)]
pub struct IntakeTake {
    /// The second every point is stamped at.
    pub timestamp: i64,
    /// The RATE interval, in seconds.
    pub interval: u32,
    pub rates: Vec<RateSeries>,
    pub sketches: Vec<SketchSeries>,
    /// Observations the batch dropped since the last take.
    pub dropped: u64,
}

impl MetricBatch {
    /// Tag every series [`MetricBatch::take_intake`] takes with this
    /// writer's `host.name` (normalized as Datadog stores a tag value) and
    /// `process.pid`, so replicas and processes never share a series.
    /// An empty host leaves it out.
    pub fn set_writer(&mut self, host: &str, pid: u32) {
        let mut tags = Vec::with_capacity(2);
        let host = datadog_tag_value(host);
        if !host.is_empty() {
            tags.push(format!("host.name:{host}"));
        }
        tags.push(format!("process.pid:{pid}"));
        self.writer_tags = tags;
    }

    /// Take every point, leaving the batch empty, as metrics intake takes it
    /// from a writer that submits to the API: Counters (the `.count` and
    /// `.sum` of distributions included) as RATE points of `sum /
    /// interval_secs`, distributions as one [`Sketch`] per series. Every
    /// point is stamped at `now_unix_seconds`, or at the second after this
    /// batch's previous take when that is later, so one writer never stamps
    /// a series twice at the same second. Tags are sorted, with the writer's
    /// (see [`MetricBatch::set_writer`]) added. An `interval_secs` of 0 is
    /// read as 1.
    pub fn take_intake(
        &mut self,
        interval_secs: u32,
        now_unix_seconds: i64,
    ) -> Result<IntakeTake, MetricError> {
        let interval = interval_secs.max(1);
        let timestamp = now_unix_seconds.max(self.next_second);
        self.next_second = timestamp.saturating_add(1);
        let dropped = self.take_dropped();
        let writer = self.writer_tags.clone();
        let taken = self.take();
        let with_writer = |mut tags: Vec<String>| {
            if !writer.is_empty() {
                tags.extend(writer.iter().cloned());
                tags.sort_unstable();
            }
            tags
        };
        let mut rates = Vec::new();
        let mut samples: BTreeMap<(String, Vec<String>), Vec<f64>> = BTreeMap::new();
        for DatadogSeries {
            name,
            series_type,
            value,
            tags,
        } in taken.datadog_series(false)?
        {
            match series_type {
                SeriesType::Count => rates.push(RateSeries {
                    metric: name,
                    tags: with_writer(tags),
                    value: value / f64::from(interval),
                }),
                SeriesType::Distribution => samples.entry((name, tags)).or_default().push(value),
            }
        }
        let sketches = samples
            .into_iter()
            .filter_map(|((metric, tags), values)| {
                Some(SketchSeries {
                    metric,
                    tags: with_writer(tags),
                    sketch: Sketch::of(&values)?,
                })
            })
            .collect();
        Ok(IntakeTake {
            timestamp,
            interval,
            rates,
            sketches,
            dropped,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keys_follow_intake() {
        // The bias puts 1e-9 in key 1's neighborhood and 1.0 at the bias.
        assert_eq!(key_bias(), 1338);
        assert_eq!(sketch_key(1.0), key_bias());
        assert_eq!(sketch_key(0.0), 0);
        assert_eq!(sketch_key(1e-12), 0);
        assert_eq!(sketch_key(-1.0), -key_bias());
        assert_eq!(sketch_key(f64::INFINITY), INF_KEY);
        assert_eq!(sketch_key(1e300), INF_KEY);
        // γ^k for whole k maps to k + bias.
        for k in [1, 10, 100, 500] {
            assert_eq!(sketch_key(gamma().powi(k)), k + key_bias(), "{k}");
        }
    }

    #[test]
    #[cfg_attr(miri, ignore)] // many encodings are too slow under Miri
    fn sketches_count_and_fold() {
        let sketch = Sketch::of(&[1.0, 1.0, 2.0, f64::NAN, 0.5, 0.0]).unwrap();
        assert_eq!(sketch.count, 5);
        assert_eq!((sketch.min, sketch.max, sketch.sum), (0.0, 2.0, 4.5));
        assert_eq!(sketch.counts.iter().sum::<u32>(), 5);
        assert_eq!(sketch.keys[0], 0);
        assert!(sketch.keys.windows(2).all(|w| w[0] < w[1]));
        assert!(Sketch::of(&[f64::NAN]).is_none());
        // Past the store's size the lowest bins fold together; no count is lost.
        let many: Vec<f64> = (0..5000).map(|i| 1.02f64.powi(i)).collect();
        let folded = Sketch::of(&many).unwrap();
        assert!(folded.keys.len() < 5000);
        assert_eq!(
            folded.counts.iter().map(|&n| u64::from(n)).sum::<u64>(),
            5000
        );
    }

    #[test]
    #[cfg_attr(miri, ignore)] // every key is too slow under Miri
    fn every_key_is_read_back_as_put() {
        for key in 1..=INF_KEY {
            let mut sketch = DDSketch::with_log_mapping(gamma(), f64::from(key_bias())).unwrap();
            sketch.add_to_bin(key, 1.0).unwrap();
            let read: Vec<i32> = sketch
                .ordered_bins()
                .into_iter()
                .filter(|(_, w)| *w > 0.0)
                .filter_map(|(value, _)| sketch.bin_index(value))
                .collect();
            assert_eq!(read, [key], "{key}");
        }
        // And a value's key is intake's.
        let sketch = Sketch::of(&[1.0, 1e-3, 42.0, 1e6]).unwrap();
        let mut expected: Vec<i32> = [1.0, 1e-3, 42.0, 1e6].map(sketch_key).to_vec();
        expected.sort_unstable();
        assert_eq!(sketch.keys, expected);
    }
}
