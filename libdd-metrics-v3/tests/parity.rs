// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! Wire-format parity tests.
//!
//! `libdd-metrics-v3` hand-rolls its own protobuf wire-format encoder (see `src/writer.rs`)
//! instead of depending on a Protocol Buffers library for efficiency purposes and to keep this
//! crate `no_std`. We check the encoder agains generated bindings in two dimensions:
//!
//! - [`assert_wire_parity`] checks *wire framing*: it decodes the hand-rolled encoder's bytes with
//!   `prost`'s generated decoder and checks the result is exactly the [`V3EncodedData`] columns we
//!   intended to encode.
//! - [`assert_points_round_trip`] checks *columnar correctness*: it decodes point/sketch values
//!   straight from the wire bytes' raw `types`/`timestamps`/`vals_*`/`sketch_*` arrays using its
//!   own delta-decoding and value-type dispatch that shares no code with `V3Writer`, and compares
//!   the result against the exact values that were passed to `add_point`/`add_sketch`.

// To check the encoder's value-type compaction boundaries
#![allow(clippy::cast_precision_loss)]
// For reference decoder check
#![allow(clippy::expect_used)]

mod pb;

use libdd_metrics_v3::{V3EncodedData, V3MetricType, V3Writer};
use prost::Message as _;

/// Builds a [`pb::MetricData`] holding the exact same columnar data as `data`.
fn to_reference_message(data: V3EncodedData) -> pb::MetricData {
    pb::MetricData {
        dict_name_str: data.dict_name_bytes,
        dict_tag_str: data.dict_tags_bytes,
        dict_tagsets: data.dict_tagsets,
        dict_resource_str: data.dict_resource_str_bytes,
        dict_resource_len: data.dict_resource_len,
        dict_resource_type: data.dict_resource_type,
        dict_resource_name: data.dict_resource_name,
        dict_source_type_name: data.dict_source_type_bytes,
        dict_origin_info: data.dict_origin_info,
        dict_unit_str: data.dict_unit_bytes,
        types: data.types,
        name_refs: data.names,
        tagset_refs: data.tags,
        resources_refs: data.resources,
        intervals: data.intervals,
        num_points: data.num_points,
        source_type_name_refs: data.source_type_names,
        origin_info_refs: data.origin_infos,
        unit_refs: data.unit_refs,
        timestamps: data.timestamps,
        vals_sint64: data.vals_sint64,
        vals_float32: data.vals_float32,
        vals_float64: data.vals_float64,
        sketch_num_bins: data.sketch_num_bins,
        sketch_bin_keys: data.sketch_bin_keys,
        sketch_bin_cnts: data.sketch_bin_cnts,
    }
}

/// Runs `build` against two independent [`V3Writer`]s: one is encoded with the hand-rolled encoder
/// under test, the other becomes the expected [`pb::MetricData`] via [`to_reference_message`].
/// Asserts that decoding the hand-rolled bytes with `prost`'s generated decoder reproduces the
/// expected message exactly, and that the two encoded lengths match (see the module docs for why
/// this checks decoded equality plus length rather than raw byte equality).
#[track_caller]
fn assert_wire_parity(build: impl Fn(&mut V3Writer)) {
    let mut ours = V3Writer::new();
    build(&mut ours);
    let ours_bytes = ours.finalize().payload;

    let mut reference = V3Writer::new();
    build(&mut reference);
    let expected = to_reference_message(reference.into_columns());

    let decoded = pb::MetricData::decode(ours_bytes.as_slice())
        .expect("hand-rolled encoder output must be valid protobuf wire format");
    assert_eq!(
        decoded, expected,
        "decoding the hand-rolled encoder's bytes must reproduce the intended message"
    );
    assert_eq!(
        ours_bytes.len(),
        expected.encoded_len(),
        "hand-rolled and prost-encoded payloads must be the same length"
    );
}

/// A point or sketch value decoded independently of `V3Writer`, identified by the bit pattern of
/// its f64 value (so NaN, which is unequal to itself under `==`, still compares correctly).
#[derive(Debug, Clone, PartialEq, Eq)]
enum DecodedPoint {
    Value {
        timestamp: i64,
        bits: u64,
    },
    Sketch {
        timestamp: i64,
        count: i64,
        sum_bits: u64,
        min_bits: u64,
        max_bits: u64,
        bin_keys: Vec<i32>,
        bin_counts: Vec<u32>,
    },
}

/// Decodes the point/sketch values of every metric in `data`, in writer-call order, straight from
/// the raw `types`/`timestamps`/`vals_*`/`sketch_*` columns — using this function's own
/// delta-decoding and value-type dispatch rather than anything from `src/writer.rs` or
/// `src/types.rs`. This is deliberately independent of `V3Writer` so that a bug in its
/// delta-encoding or value-type compaction changes this function's output without also changing
/// what it's compared against (see the module docs).
#[allow(
    clippy::cast_sign_loss,
    clippy::cast_possible_truncation,
    clippy::too_many_lines,
    clippy::panic
)]
fn decode_points(data: &pb::MetricData) -> Vec<Vec<DecodedPoint>> {
    const METRIC_TYPE_MASK: u64 = 0x0F;
    const VALUE_TYPE_MASK: u64 = 0xF0;
    const METRIC_TYPE_SKETCH: u64 = 4;

    fn delta_decode(values: &mut [i64]) {
        for i in 1..values.len() {
            values[i] += values[i - 1];
        }
    }

    fn delta_decode_i32(values: &mut [i32]) {
        for i in 1..values.len() {
            values[i] += values[i - 1];
        }
    }

    fn read_value(
        data: &pb::MetricData,
        value_type: u64,
        sint_cursor: &mut usize,
        f32_cursor: &mut usize,
        f64_cursor: &mut usize,
    ) -> f64 {
        match value_type {
            0x00 => 0.0,
            0x10 => {
                let v = data.vals_sint64[*sint_cursor];
                *sint_cursor += 1;
                v as f64
            }
            0x20 => {
                let v = data.vals_float32[*f32_cursor];
                *f32_cursor += 1;
                f64::from(v)
            }
            0x30 => {
                let v = data.vals_float64[*f64_cursor];
                *f64_cursor += 1;
                v
            }
            other => panic!("unknown v3 value type {other:#x}"),
        }
    }

    let mut timestamps = data.timestamps.clone();
    delta_decode(&mut timestamps);
    let mut sketch_bin_keys = data.sketch_bin_keys.clone();

    let (mut ts_cursor, mut sint_cursor, mut f32_cursor, mut f64_cursor) = (0, 0, 0, 0);
    let (mut sketch_point_cursor, mut sketch_key_cursor, mut sketch_cnt_cursor) = (0, 0, 0);

    data.types
        .iter()
        .enumerate()
        .map(|(i, &type_field)| {
            let metric_type = type_field & METRIC_TYPE_MASK;
            let value_type = type_field & VALUE_TYPE_MASK;
            let num_points = data.num_points[i] as usize;

            (0..num_points)
                .map(|_| {
                    let timestamp = timestamps[ts_cursor];
                    ts_cursor += 1;

                    if metric_type == METRIC_TYPE_SKETCH {
                        let sum = read_value(
                            data,
                            value_type,
                            &mut sint_cursor,
                            &mut f32_cursor,
                            &mut f64_cursor,
                        );
                        let min = read_value(
                            data,
                            value_type,
                            &mut sint_cursor,
                            &mut f32_cursor,
                            &mut f64_cursor,
                        );
                        let max = read_value(
                            data,
                            value_type,
                            &mut sint_cursor,
                            &mut f32_cursor,
                            &mut f64_cursor,
                        );
                        let count = data.vals_sint64[sint_cursor];
                        sint_cursor += 1;

                        let num_bins = data.sketch_num_bins[sketch_point_cursor] as usize;
                        sketch_point_cursor += 1;

                        let start = sketch_key_cursor;
                        delta_decode_i32(&mut sketch_bin_keys[start..start + num_bins]);
                        let bin_keys = sketch_bin_keys[start..start + num_bins].to_vec();
                        sketch_key_cursor += num_bins;

                        let bin_counts = data.sketch_bin_cnts
                            [sketch_cnt_cursor..sketch_cnt_cursor + num_bins]
                            .to_vec();
                        sketch_cnt_cursor += num_bins;

                        DecodedPoint::Sketch {
                            timestamp,
                            count,
                            sum_bits: sum.to_bits(),
                            min_bits: min.to_bits(),
                            max_bits: max.to_bits(),
                            bin_keys,
                            bin_counts,
                        }
                    } else {
                        let value = read_value(
                            data,
                            value_type,
                            &mut sint_cursor,
                            &mut f32_cursor,
                            &mut f64_cursor,
                        );
                        DecodedPoint::Value {
                            timestamp,
                            bits: value.to_bits(),
                        }
                    }
                })
                .collect()
        })
        .collect()
}

/// Runs `build` against a [`V3Writer`], and checks that independently decoding the resulting
/// hand-rolled bytes (via [`decode_points`]) reproduces exactly `expected_points` — the values
/// `build` is expected to have passed to `add_point`/`add_sketch`, in the same per-metric,
/// per-point order. Unlike [`assert_wire_parity`], this does not go through `V3Writer` on the
/// "expected" side at all, so it actually exercises delta-encoding and value-type compaction (see
/// the module docs).
#[track_caller]
fn assert_points_round_trip(build: impl Fn(&mut V3Writer), expected_points: &[Vec<DecodedPoint>]) {
    let mut writer = V3Writer::new();
    build(&mut writer);
    let bytes = writer.finalize().payload;

    let message = pb::MetricData::decode(bytes.as_slice())
        .expect("hand-rolled encoder output must be valid protobuf");
    let decoded = decode_points(&message);

    assert_eq!(
        decoded, expected_points,
        "decoding the hand-rolled encoder's bytes must reproduce the original point values"
    );
}

#[test]
fn empty_payload() {
    assert_wire_parity(|_writer| {});
}

#[test]
fn single_gauge_zero_value() {
    let build = |writer: &mut V3Writer| {
        let mut m = writer.write(V3MetricType::Gauge, "zero.metric");
        m.add_point(1_000, 0.0).unwrap();
        m.close();
    };
    assert_wire_parity(build);
    assert_points_round_trip(
        build,
        &[vec![DecodedPoint::Value {
            timestamp: 1_000,
            bits: 0.0_f64.to_bits(),
        }]],
    );
}

#[test]
fn single_count_small_int_value() {
    let build = |writer: &mut V3Writer| {
        let mut m = writer.write(V3MetricType::Count, "small.int");
        m.add_point(1_000, 42.0).unwrap();
        m.close();
    };
    assert_wire_parity(build);
    assert_points_round_trip(
        build,
        &[vec![DecodedPoint::Value {
            timestamp: 1_000,
            bits: 42.0_f64.to_bits(),
        }]],
    );
}

#[test]
fn single_gauge_large_int_value() {
    // Larger than 2^24 but still losslessly representable as sint64.
    let value = (1i64 << 40) as f64;
    let build = move |writer: &mut V3Writer| {
        let mut m = writer.write(V3MetricType::Gauge, "large.int");
        m.add_point(1_000, value).unwrap();
        m.close();
    };
    assert_wire_parity(build);
    assert_points_round_trip(
        build,
        &[vec![DecodedPoint::Value {
            timestamp: 1_000,
            bits: value.to_bits(),
        }]],
    );
}

#[test]
fn single_gauge_float32_value() {
    let build = |writer: &mut V3Writer| {
        let mut m = writer.write(V3MetricType::Gauge, "float32.metric");
        m.add_point(1_000, 1.5).unwrap();
        m.close();
    };
    assert_wire_parity(build);
    assert_points_round_trip(
        build,
        &[vec![DecodedPoint::Value {
            timestamp: 1_000,
            bits: 1.5_f64.to_bits(),
        }]],
    );
}

#[test]
fn single_gauge_float64_value() {
    let build = |writer: &mut V3Writer| {
        let mut m = writer.write(V3MetricType::Gauge, "float64.metric");
        m.add_point(1_000, core::f64::consts::PI).unwrap();
        m.close();
    };
    assert_wire_parity(build);
    assert_points_round_trip(
        build,
        &[vec![DecodedPoint::Value {
            timestamp: 1_000,
            bits: core::f64::consts::PI.to_bits(),
        }]],
    );
}

#[test]
fn nan_value_round_trips_as_float64() {
    // NaN is a legitimate (if unusual) point value: producers can submit it, and this crate must
    // encode it deterministically rather than panicking or silently substituting another value.
    // `assert_wire_parity` can't express this case: `MetricData`'s derived `PartialEq` (like IEEE
    // 754 itself) treats NaN as unequal to itself, so it would fail even on a correct encoding.
    // `assert_points_round_trip` compares bit patterns instead, so it can actually check this.
    let build = |writer: &mut V3Writer| {
        let mut m = writer.write(V3MetricType::Gauge, "nan.metric");
        m.add_point(1_000, f64::NAN).unwrap();
        m.close();
    };
    assert_points_round_trip(
        build,
        &[vec![DecodedPoint::Value {
            timestamp: 1_000,
            bits: f64::NAN.to_bits(),
        }]],
    );
}

#[test]
fn mixed_large_int_and_float32_promotes_to_float64() {
    // Regression case: a large integer mixed with a fractional float32 value must be stored (and
    // therefore wire-encoded) as float64 to avoid precision loss. `large` is deliberately not a
    // power of two: 2^30 itself happens to survive an f32 round-trip losslessly (only the
    // exponent is used, the mantissa is all zero), so it wouldn't actually detect a regression
    // where this promotion is missing and everything gets compacted to float32 instead. 2^30 + 1
    // needs 31 significant bits and does not fit in f32's 24-bit mantissa, so a missing promotion
    // would visibly corrupt this value.
    let large = ((1i64 << 30) + 1) as f64;
    let build = move |writer: &mut V3Writer| {
        let mut m = writer.write(V3MetricType::Gauge, "mixed.metric");
        m.add_point(1_000, large).unwrap();
        m.add_point(2_000, 1.5).unwrap();
        m.close();
    };
    assert_wire_parity(build);
    assert_points_round_trip(
        build,
        &[vec![
            DecodedPoint::Value {
                timestamp: 1_000,
                bits: large.to_bits(),
            },
            DecodedPoint::Value {
                timestamp: 2_000,
                bits: 1.5_f64.to_bits(),
            },
        ]],
    );
}

#[test]
fn rate_metric_with_interval() {
    let build = |writer: &mut V3Writer| {
        let mut m = writer.write(V3MetricType::Rate, "rate.metric");
        m.set_interval(60);
        m.add_point(1_000, 3.5).unwrap();
        m.close();
    };
    assert_wire_parity(build);
    assert_points_round_trip(
        build,
        &[vec![DecodedPoint::Value {
            timestamp: 1_000,
            bits: 3.5_f64.to_bits(),
        }]],
    );
}

#[test]
fn multiple_points_per_metric() {
    let build = |writer: &mut V3Writer| {
        let mut m = writer.write(V3MetricType::Gauge, "multi.point");
        for i in 0..10 {
            m.add_point(1_000 + i * 10, i as f64).unwrap();
        }
        m.close();
    };
    assert_wire_parity(build);
    assert_points_round_trip(
        build,
        &[(0..10)
            .map(|i| DecodedPoint::Value {
                timestamp: 1_000 + i * 10,
                bits: (i as f64).to_bits(),
            })
            .collect()],
    );
}

#[test]
fn multiple_metrics_share_interned_name() {
    assert_wire_parity(|writer| {
        for i in 0u32..3 {
            let mut m = writer.write(V3MetricType::Count, "shared.name");
            m.add_point(1_000 + i64::from(i), f64::from(i)).unwrap();
            m.close();
        }
    });
}

#[test]
fn tags_are_deduplicated_across_metrics() {
    assert_wire_parity(|writer| {
        {
            let mut m = writer.write(V3MetricType::Gauge, "a");
            m.set_tags(["env:prod", "service:web"].into_iter());
            m.add_point(1_000, 1.0).unwrap();
            m.close();
        }
        {
            // Overlapping tag (env:prod) plus a new one; exercises both dictionary reuse and a
            // brand-new tagset.
            let mut m = writer.write(V3MetricType::Gauge, "b");
            m.set_tags(["env:prod", "service:api"].into_iter());
            m.add_point(2_000, 2.0).unwrap();
            m.close();
        }
        {
            // Exact same tagset as the first metric; exercises tagset (not just tag) dedup.
            let mut m = writer.write(V3MetricType::Gauge, "c");
            m.set_tags(["env:prod", "service:web"].into_iter());
            m.add_point(3_000, 3.0).unwrap();
            m.close();
        }
        {
            let mut m = writer.write(V3MetricType::Gauge, "no.tags");
            m.add_point(4_000, 4.0).unwrap();
            m.close();
        }
    });
}

#[test]
fn resources_host_and_device_pairs() {
    assert_wire_parity(|writer| {
        {
            let mut m = writer.write(V3MetricType::Gauge, "with.resources");
            m.set_resources(&[("host", "server-1"), ("device", "eth0")]);
            m.add_point(1_000, 1.0).unwrap();
            m.close();
        }
        {
            // Same resource set again: exercises resource-set dedup.
            let mut m = writer.write(V3MetricType::Gauge, "same.resources");
            m.set_resources(&[("host", "server-1"), ("device", "eth0")]);
            m.add_point(2_000, 2.0).unwrap();
            m.close();
        }
        {
            let mut m = writer.write(V3MetricType::Gauge, "no.resources");
            m.add_point(3_000, 3.0).unwrap();
            m.close();
        }
    });
}

#[test]
fn source_type_name_is_encoded() {
    assert_wire_parity(|writer| {
        let mut m = writer.write(V3MetricType::Count, "with.source.type");
        m.set_source_type("nginx");
        m.add_point(1_000, 1.0).unwrap();
        m.close();
    });
}

#[test]
fn origin_metadata_with_no_index_flag() {
    assert_wire_parity(|writer| {
        let mut m = writer.write(V3MetricType::Gauge, "with.origin");
        m.set_origin(1, 2, 3, true);
        m.add_point(1_000, 1.0).unwrap();
        m.close();
    });
}

#[test]
fn unit_toggled_on_off_on() {
    assert_wire_parity(|writer| {
        {
            let mut m = writer.write(V3MetricType::Gauge, "has.unit");
            m.set_unit("millisecond");
            m.add_point(1_000, 42.0).unwrap();
            m.close();
        }
        {
            let mut m = writer.write(V3MetricType::Gauge, "no.unit");
            m.add_point(1_000, 43.0).unwrap();
            m.close();
        }
        {
            // Reuses the "millisecond" unit dictionary entry.
            let mut m = writer.write(V3MetricType::Gauge, "same.unit");
            m.set_unit("millisecond");
            m.add_point(1_000, 44.0).unwrap();
            m.close();
        }
    });
}

#[test]
fn unit_set_then_cleared() {
    assert_wire_parity(|writer| {
        let mut m = writer.write(V3MetricType::Gauge, "cleared.unit");
        m.set_unit("byte");
        m.set_unit(""); // clears it back out
        m.add_point(1_000, 1.0).unwrap();
        m.close();
    });
}

#[test]
fn sketch_with_integer_summary() {
    let build = |writer: &mut V3Writer| {
        let mut m = writer.write(V3MetricType::Sketch, "sketch.int");
        m.add_sketch(
            1_000,
            5,
            15.0,
            1.0,
            9.0,
            &[-2, -1, 0, 1, 2],
            &[1, 1, 1, 1, 1],
        )
        .unwrap();
        m.close();
    };
    assert_wire_parity(build);
    assert_points_round_trip(
        build,
        &[vec![DecodedPoint::Sketch {
            timestamp: 1_000,
            count: 5,
            sum_bits: 15.0_f64.to_bits(),
            min_bits: 1.0_f64.to_bits(),
            max_bits: 9.0_f64.to_bits(),
            bin_keys: vec![-2, -1, 0, 1, 2],
            bin_counts: vec![1, 1, 1, 1, 1],
        }]],
    );
}

#[test]
fn sketch_with_float_summary() {
    let build = |writer: &mut V3Writer| {
        let mut m = writer.write(V3MetricType::Sketch, "sketch.float");
        m.add_sketch(
            1_000,
            3,
            4.5,
            0.5,
            core::f64::consts::E,
            &[-1, 0, 1],
            &[2, 3, 1],
        )
        .unwrap();
        m.close();
    };
    assert_wire_parity(build);
    assert_points_round_trip(
        build,
        &[vec![DecodedPoint::Sketch {
            timestamp: 1_000,
            count: 3,
            sum_bits: 4.5_f64.to_bits(),
            min_bits: 0.5_f64.to_bits(),
            max_bits: core::f64::consts::E.to_bits(),
            bin_keys: vec![-1, 0, 1],
            bin_counts: vec![2, 3, 1],
        }]],
    );
}

#[test]
fn sketch_with_multiple_points() {
    let build = |writer: &mut V3Writer| {
        let mut m = writer.write(V3MetricType::Sketch, "sketch.multi");
        m.add_sketch(1_000, 2, 3.0, 1.0, 2.0, &[0, 1], &[1, 1])
            .unwrap();
        m.add_sketch(2_000, 4, 20.0, 1.0, 15.0, &[-3, -2, -1, 0], &[1, 1, 1, 1])
            .unwrap();
        m.close();
    };
    assert_wire_parity(build);
    assert_points_round_trip(
        build,
        &[vec![
            DecodedPoint::Sketch {
                timestamp: 1_000,
                count: 2,
                sum_bits: 3.0_f64.to_bits(),
                min_bits: 1.0_f64.to_bits(),
                max_bits: 2.0_f64.to_bits(),
                bin_keys: vec![0, 1],
                bin_counts: vec![1, 1],
            },
            DecodedPoint::Sketch {
                timestamp: 2_000,
                count: 4,
                sum_bits: 20.0_f64.to_bits(),
                min_bits: 1.0_f64.to_bits(),
                max_bits: 15.0_f64.to_bits(),
                bin_keys: vec![-3, -2, -1, 0],
                bin_counts: vec![1, 1, 1, 1],
            },
        ]],
    );
}

#[test]
fn kitchen_sink_payload() {
    // Combines every dimension above into one payload: multiple metric types, shared and
    // divergent tag/resource/unit/origin/source-type dictionaries, every value-type compaction
    // path, and both point and sketch metrics.
    assert_wire_parity(|writer| {
        {
            let mut m = writer.write(V3MetricType::Count, "requests.count");
            m.set_tags(["env:prod", "service:web", "region:us-east"].into_iter());
            m.set_resources(&[("host", "server-1")]);
            m.set_source_type("nginx");
            m.add_point(1_000, 0.0).unwrap();
            m.add_point(1_010, 12.0).unwrap();
            m.close();
        }
        {
            let mut m = writer.write(V3MetricType::Rate, "requests.rate");
            m.set_tags(["env:prod", "service:web"].into_iter());
            m.set_interval(10);
            m.set_unit("request");
            m.add_point(1_000, 1.2).unwrap();
            m.close();
        }
        {
            let mut m = writer.write(V3MetricType::Gauge, "memory.usage");
            m.set_tags(["env:prod", "service:api"].into_iter());
            m.set_resources(&[("host", "server-1"), ("container", "abc123")]);
            m.set_unit("byte");
            m.set_origin(7, 2, 1, false);
            m.add_point(1_000, (1i64 << 32) as f64).unwrap();
            m.close();
        }
        {
            let mut m = writer.write(V3MetricType::Gauge, "cpu.usage");
            m.set_tags(core::iter::once("env:staging"));
            m.set_origin(7, 2, 1, true); // reuses origin dict entry, sets no-index flag
            m.add_point(1_000, 0.42).unwrap();
            m.add_point(1_010, (1i64 << 30) as f64).unwrap(); // forces float64 alongside a fraction below
            m.add_point(1_020, 0.5).unwrap();
            m.close();
        }
        {
            let mut m = writer.write(V3MetricType::Sketch, "latency.distribution");
            m.set_tags(["env:prod", "service:web"].into_iter()); // reuses an earlier tagset
            m.set_unit("millisecond");
            m.add_sketch(
                1_000,
                5,
                25.0,
                1.0,
                12.0,
                &[-2, -1, 0, 1, 2],
                &[1, 1, 1, 1, 1],
            )
            .unwrap();
            m.add_sketch(
                2_000,
                3,
                4.5,
                0.5,
                core::f64::consts::E,
                &[-1, 0, 1],
                &[2, 3, 1],
            )
            .unwrap();
            m.close();
        }
        {
            // No tags, no resources, no unit, no origin, no source type: exercises every "0 =
            // empty" dictionary reference path in the same payload as everything above.
            let mut m = writer.write(V3MetricType::Gauge, "bare.metric");
            m.add_point(1_000, 99.0).unwrap();
            m.close();
        }
    });
}

/// Randomized coverage of the writer/encoder's most intricate logic: name and tag interning
/// (dictionary reuse across metrics), delta encoding of the resulting reference columns, and
/// value-type compaction across the zero/int24/int48/float32/float64 boundaries. Each generated
/// case is checked for the same byte-for-byte parity as the examples above.
#[test]
fn wire_bytes_match_prost_for_randomized_metrics() {
    use bolero::TypeGenerator as _;

    const NAME_POOL: &[&str] = &["requests", "latency", "errors", "cpu.usage", "memory.usage"];
    const TAG_POOL: &[&str] = &[
        "env:prod",
        "env:staging",
        "service:web",
        "service:api",
        "region:us-east",
    ];

    let metric_type_idx = 0u8..=2; // Count, Rate, Gauge (sketches are covered by dedicated tests above)
    let name_idx = 0usize..NAME_POOL.len();
    let tag_idx = 0usize..TAG_POOL.len();
    let tags = Vec::<usize>::produce().with().values(tag_idx);
    let values = Vec::<f64>::produce();
    let metrics = Vec::<(u8, usize, Vec<usize>, Vec<f64>)>::produce()
        .with()
        .values((metric_type_idx, name_idx, tags, values));

    bolero::check!()
        .with_generator(metrics)
        .for_each(|metrics| {
            // NaN is scrubbed out up front (and shared between the writer and the expectations
            // below): it's encoded deterministically like any other value, but `MetricData`'s
            // derived `PartialEq` (like IEEE 754 itself) treats NaN as unequal to itself, which
            // would make the equality checks below spuriously fail on a
            // correctly-encoded payload.
            let metrics: Vec<(u8, usize, Vec<usize>, Vec<f64>)> = metrics
                .iter()
                .map(|(type_idx, name_idx, tag_idxs, values)| {
                    let values = values
                        .iter()
                        .map(|v| if v.is_nan() { 0.0 } else { *v })
                        .collect();
                    (*type_idx, *name_idx, tag_idxs.clone(), values)
                })
                .collect();

            let build = |writer: &mut V3Writer| {
                for (type_idx, name_idx, tag_idxs, values) in &metrics {
                    let metric_type = match type_idx % 3 {
                        0 => V3MetricType::Count,
                        1 => V3MetricType::Rate,
                        _ => V3MetricType::Gauge,
                    };
                    let mut m = writer.write(metric_type, NAME_POOL[*name_idx]);
                    m.set_tags(tag_idxs.iter().map(|&i| TAG_POOL[i]));
                    m.set_interval(7);
                    for (i, &value) in values.iter().enumerate() {
                        // Non-negative, monotonically increasing timestamps (`i` is bounded by the
                        // generated `Vec`'s length, nowhere near overflowing); the actual values
                        // are what's under test here.
                        #[allow(clippy::cast_possible_wrap)]
                        m.add_point(1_000 + i as i64, value).unwrap();
                    }
                    m.close();
                }
            };
            assert_wire_parity(build);

            let expected_points: Vec<Vec<DecodedPoint>> = metrics
                .iter()
                .map(|(_, _, _, values)| {
                    values
                        .iter()
                        .enumerate()
                        .map(|(i, value)| DecodedPoint::Value {
                            #[allow(clippy::cast_possible_wrap)]
                            timestamp: 1_000 + i as i64,
                            bits: value.to_bits(),
                        })
                        .collect()
                })
                .collect();
            assert_points_round_trip(build, &expected_points);
        });
}
