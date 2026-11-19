// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! A batch as Datadog metrics intake takes it from a writer that submits
//! straight to the API (a gateway, not through the Agent): Counters as RATE
//! points over [`DATADOG_RATE_INTERVAL_SECS`], the interval the Agent stores
//! DogStatsD counts with, distributions as one DDSketch per series
//! in intake's `sketchpb.Sparse` form, stamped at a second no earlier flush
//! of the same writer used, and tagged with the writer's identity.
//!
//! Metrics intake keeps one write per metric, tag set and timestamp, so two
//! writers must never share a series (`host.name` and `process.pid` keep
//! them apart) and one writer must never stamp a series twice at the same
//! second.

use libdd_ddsketch::DDSketch;
use serde::{Serialize, Serializer};

use crate::MetricBatch;
use crate::datadog::{DATADOG_RATE_INTERVAL_SECS, DatadogSeries, SeriesType, datadog_tag_value};
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
/// by [`DATADOG_RATE_INTERVAL_SECS`], to be submitted with type RATE and
/// that interval, and aggregated by SUM across writers.
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
///
/// Serialized as a writer submits it: `{"distributions": [{"metric",
/// "sketch", "tags"}], "dropped", "interval", "series": [{"interval",
/// "metric", "tags", "type", "value"}], "timestamp"}`. `series` are the
/// [`IntakeTake::rates`], each `"type": "rate"` with the take's `interval`;
/// `distributions` the [`IntakeTake::sketches`], `sketch` being the standard
/// base64 (with padding) of [`Sketch::encode_sparse`].
#[derive(Debug, Clone, PartialEq)]
pub struct IntakeTake {
    /// The second every point is stamped at.
    pub timestamp: i64,
    /// The RATE interval, in seconds: always [`DATADOG_RATE_INTERVAL_SECS`].
    pub interval: u32,
    pub rates: Vec<RateSeries>,
    pub sketches: Vec<SketchSeries>,
    /// Observations the batch dropped since the last take.
    pub dropped: u64,
}

/// Standard base64 with padding, for binary values inside JSON output.
fn base64(bytes: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let b = [
            chunk[0],
            chunk.get(1).copied().unwrap_or(0),
            chunk.get(2).copied().unwrap_or(0),
        ];
        let n = (u32::from(b[0]) << 16) | (u32::from(b[1]) << 8) | u32::from(b[2]);
        for (i, shift) in [18, 12, 6, 0].into_iter().enumerate() {
            if i <= chunk.len() {
                out.push(char::from(ALPHABET[((n >> shift) & 63) as usize]));
            } else {
                out.push('=');
            }
        }
    }
    out
}

/// A [`RateSeries`] as [`IntakeTake`] serializes it.
#[derive(Serialize)]
struct RatePoint<'a> {
    interval: u32,
    metric: &'a str,
    tags: &'a [String],
    #[serde(rename = "type")]
    series_type: &'static str,
    value: f64,
}

/// A [`SketchSeries`] as [`IntakeTake`] serializes it.
#[derive(Serialize)]
struct SketchPoint<'a> {
    metric: &'a str,
    sketch: String,
    tags: &'a [String],
}

#[derive(Serialize)]
struct IntakeBody<'a> {
    distributions: Vec<SketchPoint<'a>>,
    dropped: u64,
    interval: u32,
    series: Vec<RatePoint<'a>>,
    timestamp: i64,
}

impl Serialize for IntakeTake {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        IntakeBody {
            distributions: self
                .sketches
                .iter()
                .map(|s| SketchPoint {
                    metric: &s.metric,
                    sketch: base64(&s.sketch.encode_sparse()),
                    tags: &s.tags,
                })
                .collect(),
            dropped: self.dropped,
            interval: self.interval,
            series: self
                .rates
                .iter()
                .map(|s| RatePoint {
                    interval: self.interval,
                    metric: &s.metric,
                    tags: &s.tags,
                    series_type: SeriesType::Rate.as_str(),
                    value: s.value,
                })
                .collect(),
            timestamp: self.timestamp,
        }
        .serialize(serializer)
    }
}

/// The system clock's current Unix second: 0 before 1970, `i64::MAX` past
/// its range.
fn unix_seconds_now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |elapsed| {
            i64::try_from(elapsed.as_secs()).unwrap_or(i64::MAX)
        })
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

    /// [`MetricBatch::take_intake`] at the system clock's current second.
    /// The one call of this crate that reads the clock.
    pub fn take_intake_now(&mut self) -> Result<IntakeTake, MetricError> {
        self.take_intake(unix_seconds_now())
    }

    /// Take every point, leaving the batch empty, as metrics intake takes it
    /// from a writer that submits to the API: Counters (the `.count` and
    /// `.sum` of distributions included) as RATE points of `sum / 10` over
    /// [`DATADOG_RATE_INTERVAL_SECS`], distributions as one [`Sketch`] per
    /// series. Every point is stamped at `now_unix_seconds`, or at the second
    /// after this batch's previous take when that is later, so one writer
    /// never stamps a series twice at the same second. Tags are sorted, with
    /// the writer's (see [`MetricBatch::set_writer`]) added.
    ///
    /// The interval is 10 s whatever the time between takes: it is part of
    /// the metric's stored type, which every writer of the metric must state
    /// alike, and `as_count()` reads each point back as `value * 10`, the sum
    /// since the last take. A writer that takes every 10 s also gives the
    /// per-second values the Agent gives.
    pub fn take_intake(&mut self, now_unix_seconds: i64) -> Result<IntakeTake, MetricError> {
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
        let (counts, distributions) = taken.datadog_parts()?;
        let rates = counts
            .into_iter()
            .filter(|series| series.series_type == SeriesType::Rate)
            .map(
                |DatadogSeries {
                     name, value, tags, ..
                 }| RateSeries {
                    metric: name,
                    tags: with_writer(tags),
                    value,
                },
            )
            .collect();
        let mut sketches: Vec<SketchSeries> = distributions
            .into_iter()
            .filter_map(|held| {
                Some(SketchSeries {
                    sketch: Sketch::of(&held.samples)?,
                    metric: held.name,
                    tags: with_writer(held.tags),
                })
            })
            .collect();
        sketches.sort_by(|a, b| a.metric.cmp(&b.metric).then_with(|| a.tags.cmp(&b.tags)));
        Ok(IntakeTake {
            timestamp,
            interval: DATADOG_RATE_INTERVAL_SECS,
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
    fn base64_is_standard_and_padded() {
        assert_eq!(base64(b""), "");
        assert_eq!(base64(b"f"), "Zg==");
        assert_eq!(base64(b"fo"), "Zm8=");
        assert_eq!(base64(b"foo"), "Zm9v");
        assert_eq!(base64(b"foob"), "Zm9vYg==");
        assert_eq!(base64(&[0xff, 0xfe, 0x00]), "//4A");
    }

    #[test]
    fn a_take_serializes_as_submitted() {
        let sketch = Sketch::of(&[1.0]).unwrap();
        let take = IntakeTake {
            timestamp: 1_700_000_000,
            interval: DATADOG_RATE_INTERVAL_SECS,
            rates: vec![RateSeries {
                metric: "c".to_string(),
                tags: vec!["a:b".to_string()],
                value: 0.5,
            }],
            sketches: vec![SketchSeries {
                metric: "d".to_string(),
                tags: vec![],
                sketch: sketch.clone(),
            }],
            dropped: 2,
        };
        let text = serde_json::to_string(&take).unwrap();
        assert_eq!(
            text,
            format!(
                concat!(
                    r#"{{"distributions":[{{"metric":"d","sketch":"{}","tags":[]}}],"#,
                    r#""dropped":2,"interval":10,"#,
                    r#""series":[{{"interval":10,"metric":"c","tags":["a:b"],"type":"rate","value":0.5}}],"#,
                    r#""timestamp":1700000000}}"#
                ),
                base64(&sketch.encode_sparse())
            )
        );
        // Keys are written sorted, as a `serde_json::Value` writes them.
        assert_eq!(serde_json::to_value(&take).unwrap().to_string(), text);
    }

    #[test]
    fn a_take_now_reads_the_clock() {
        let mut batch = MetricBatch::new();
        let taken = batch.take_intake_now().unwrap();
        assert!(taken.timestamp > 1_700_000_000);
        // The next take is stamped later, whatever the clock says.
        assert!(batch.take_intake(0).unwrap().timestamp > taken.timestamp);
    }

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
