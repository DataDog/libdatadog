// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use super::*;
use libdd_tinybytes::BytesString;
use libdd_trace_utils::span::v04::SpanBytes;

fn chunk(id: u64, size: usize) -> Vec<SpanBytes> {
    vec![SpanBytes {
        trace_id: u128::from(id),
        span_id: id,
        resource: BytesString::from(format!("{id}:{}", "x".repeat(size))),
        ..Default::default()
    }]
}

fn encoded(traces: &[Vec<SpanBytes>], format: TraceExporterOutputFormat) -> Vec<u8> {
    let mut bytes = Vec::new();
    TraceSerializer::encode_to_writer(&mut bytes, traces, &TracerMetadata::default(), format, None)
        .unwrap();
    bytes
}

#[test]
fn bounds_all_formats_and_preserves_encoding_and_order() {
    for format in [
        TraceExporterOutputFormat::V04,
        TraceExporterOutputFormat::V05,
        TraceExporterOutputFormat::V1,
    ] {
        let traces: Vec<_> = (1..10).map(|id| chunk(id, 128)).collect();
        let limit = traces
            .iter()
            .map(|chunk| encoded(std::slice::from_ref(chunk), format).len())
            .max()
            .unwrap()
            * 2;
        let metadata = TracerMetadata::default();
        let mut seen = Vec::new();
        for payload in Payloads::new(&traces, &metadata, format, limit) {
            let Payload::Encoded { indices, bytes } = payload else {
                panic!("unexpected drop");
            };
            assert!(bytes.len() <= limit);
            let selected: Vec<_> = indices.iter().map(|&index| traces[index].clone()).collect();
            assert_eq!(bytes, encoded(&selected, format));
            seen.extend(indices);
        }
        assert_eq!(seen, (0..9).collect::<Vec<_>>());
    }
}

#[test]
fn packs_survivors_across_an_oversized_chunk() {
    for format in [
        TraceExporterOutputFormat::V04,
        TraceExporterOutputFormat::V05,
        TraceExporterOutputFormat::V1,
    ] {
        let mut traces: Vec<_> = (0..1024).map(|id| chunk(id, 8)).collect();
        traces[500] = chunk(500, DEFAULT_MAX_PAYLOAD_BYTES + 1);
        let metadata = TracerMetadata::default();
        let mut requests = 0;
        let mut seen = Vec::new();
        for payload in Payloads::new(&traces, &metadata, format, DEFAULT_MAX_PAYLOAD_BYTES) {
            match payload {
                Payload::Encoded { indices, .. } => {
                    requests += 1;
                    seen.extend(indices);
                }
                Payload::Oversized { index } => assert_eq!(index, 500),
                _ => panic!("unexpected outcome"),
            }
        }
        assert_eq!(requests, 1);
        assert_eq!(seen, (0..1024).filter(|&i| i != 500).collect::<Vec<_>>());
    }
}

#[test]
fn packs_large_prefix_without_fragmenting_the_small_tail() {
    let mut traces: Vec<_> = (0..1024).map(|id| chunk(id, 8)).collect();
    traces[0] = chunk(0, 3 * 1024 * 1024);
    traces[1] = chunk(1, 3 * 1024 * 1024);
    let metadata = TracerMetadata::default();
    assert_eq!(
        Payloads::new(
            &traces,
            &metadata,
            TraceExporterOutputFormat::V04,
            DEFAULT_MAX_PAYLOAD_BYTES
        )
        .count(),
        2
    );
}

#[test]
fn exact_limits_include_envelopes() {
    for format in [
        TraceExporterOutputFormat::V04,
        TraceExporterOutputFormat::V05,
        TraceExporterOutputFormat::V1,
    ] {
        let traces = vec![chunk(1, 32)];
        let size = encoded(&traces, format).len();
        let metadata = TracerMetadata::default();
        for limit in [0, size - 1, size, size + 1] {
            let mut payloads = Payloads::new(&traces, &metadata, format, limit);
            assert_eq!(
                matches!(payloads.next().unwrap(), Payload::Encoded { .. }),
                limit >= size
            );
            assert!(payloads.next().is_none());
        }
    }
}

#[test]
fn stops_during_metadata_preparation() {
    use std::sync::atomic::{AtomicUsize, Ordering};
    let traces: Vec<_> = (0..1000).map(|id| chunk(id, 8)).collect();
    let checks = AtomicUsize::new(0);
    let stop = || checks.fetch_add(1, Ordering::Relaxed) > 10;
    let metadata = TracerMetadata::default();
    let mut payloads = Payloads::new(&traces, &metadata, TraceExporterOutputFormat::V1, 1024)
        .with_limits(1024, &stop);
    assert!(matches!(payloads.next(), Some(Payload::Stopped)));
    assert!(checks.load(Ordering::Relaxed) < 20);
}

#[test]
fn a_warmed_capacity_hint_avoids_output_growth() {
    let traces: Vec<_> = (0..1000).map(|id| chunk(id, 128)).collect();
    let format = TraceExporterOutputFormat::V04;
    let size = encoded(&traces, format).len();
    assert!(!size.is_power_of_two());
    let metadata = TracerMetadata::default();
    let stop = || false;
    let mut payloads = Payloads::new(&traces, &metadata, format, DEFAULT_MAX_PAYLOAD_BYTES)
        .with_limits(size, &stop);
    let Payload::Encoded { bytes, .. } = payloads.next().unwrap() else {
        panic!("expected a payload");
    };
    assert_eq!(bytes.len(), size);
    assert_eq!(bytes.capacity(), size);
    assert!(payloads.next().is_none());
}

#[test]
fn empty_v1_chunks_keep_builder_metadata() {
    let traces: Vec<Vec<SpanBytes>> = vec![vec![], vec![]];
    let metadata = TracerMetadata {
        env: "production".to_owned(),
        hostname: "host".to_owned(),
        ..Default::default()
    };
    let expected = libdd_trace_utils::msgpack_encoder::v1::to_vec_from_v04(&traces, &metadata);
    let mut payloads = Payloads::new(&traces, &metadata, TraceExporterOutputFormat::V1, 1024);
    let Payload::Encoded { bytes, .. } = payloads.next().unwrap() else {
        panic!("expected a payload");
    };
    assert_eq!(bytes, expected);
}

#[test]
fn many_tiny_v04_chunks_are_packed_without_prefix_rescans() {
    use std::sync::atomic::{AtomicUsize, Ordering};
    let traces = vec![
        vec![SpanBytes {
            resource: BytesString::from_static("small"),
            ..Default::default()
        }];
        65_536
    ];
    let total = encoded(&traces, TraceExporterOutputFormat::V04).len();
    assert!(total > DEFAULT_MAX_PAYLOAD_BYTES && total < DEFAULT_MAX_PAYLOAD_BYTES * 2);
    let checks = AtomicUsize::new(0);
    let stop = || {
        checks.fetch_add(1, Ordering::Relaxed);
        false
    };
    let metadata = TracerMetadata::default();
    let payloads = Payloads::new(
        &traces,
        &metadata,
        TraceExporterOutputFormat::V04,
        DEFAULT_MAX_PAYLOAD_BYTES,
    )
    .with_limits(1024, &stop);
    let mut requests = 0;
    for payload in payloads {
        let Payload::Encoded { bytes, .. } = payload else {
            panic!("unexpected drop");
        };
        assert!(bytes.len() <= DEFAULT_MAX_PAYLOAD_BYTES);
        requests += 1;
    }
    assert_eq!(requests, 2);
    assert!(checks.load(Ordering::Relaxed) < traces.len() * 6);
}

#[test]
fn v04_packing_accounts_for_array_header_transitions() {
    for count in [15, 16, 65_535, 65_536] {
        let survivors = vec![vec![SpanBytes::default()]; count];
        let expected = encoded(&survivors, TraceExporterOutputFormat::V04);
        let mut traces = vec![chunk(1, expected.len() + 1)];
        traces.extend(survivors);
        let metadata = TracerMetadata::default();
        let mut payloads = Payloads::new(
            &traces,
            &metadata,
            TraceExporterOutputFormat::V04,
            expected.len(),
        );
        assert!(matches!(
            payloads.next(),
            Some(Payload::Oversized { index: 0 })
        ));
        let Some(Payload::Encoded { indices, bytes }) = payloads.next() else {
            panic!("expected packed survivors");
        };
        assert_eq!(indices.len(), count);
        assert_eq!(bytes, expected);
        assert!(payloads.next().is_none());
    }
}

#[test]
fn mid_write_stops_are_terminal_for_output_and_counting() {
    use std::sync::atomic::{AtomicUsize, Ordering};
    for format in [
        TraceExporterOutputFormat::V04,
        TraceExporterOutputFormat::V1,
        TraceExporterOutputFormat::V05,
    ] {
        for keep in [false, true] {
            let traces: Vec<_> = (0..4).map(|id| chunk(id, 96 * 1024)).collect();
            let checks = AtomicUsize::new(0);
            let stop = || {
                let n = checks.fetch_add(1, Ordering::Relaxed);
                // Fail promptly if write_all starts retrying a terminal stop.
                assert!(n < 3, "encoding retried cancellation");
                n > 0
            };
            let limit = Limit {
                limit: 512 * 1024,
                exceeded: false,
                stopped: false,
                next_check: 64 * 1024,
                stop: &stop,
            };
            let metadata = TracerMetadata::default();
            let (result, length, limit) = if keep {
                let mut writer = LimitedBuffer {
                    bytes: Vec::new(),
                    limit,
                };
                let result = TraceSerializer::encode_to_writer(
                    &mut writer,
                    &traces,
                    &metadata,
                    format,
                    None,
                );
                (result, writer.bytes.len(), writer.limit)
            } else {
                let mut writer = Counter { length: 0, limit };
                let result = TraceSerializer::encode_to_writer(
                    &mut writer,
                    &traces,
                    &metadata,
                    format,
                    None,
                );
                (result, writer.length, writer.limit)
            };
            assert!(length >= 64 * 1024);
            assert!(limit.stopped);
            assert!(matches!(
                finish_encoding(result, limit, length, Vec::new()),
                Encoded::Stopped
            ));
            assert_eq!(checks.load(Ordering::Relaxed), 2);
        }
    }
}

#[test]
fn exhausted_planners_do_not_report_late_cancellation() {
    use std::sync::atomic::{AtomicBool, Ordering};
    for traces in [
        vec![],
        vec![chunk(1, 16)],
        vec![chunk(1, 1024)],
        vec![chunk(1, 128), chunk(2, 128)],
    ] {
        let stopped = AtomicBool::new(false);
        let stop = || stopped.load(Ordering::Relaxed);
        let metadata = TracerMetadata::default();
        let mut planner = Payloads::new(&traces, &metadata, TraceExporterOutputFormat::V04, 256)
            .with_limits(256, &stop);
        while planner.next().is_some() {}
        stopped.store(true, Ordering::Relaxed);
        assert!(planner.next().is_none());
    }
}

#[test]
fn fallback_only_replans_unsent_survivors() {
    let mut traces: Vec<_> = (0..4).map(|id| chunk(id, 300)).collect();
    traces.insert(2, chunk(10, 2048));
    let metadata = TracerMetadata::default();
    let mut planner = Payloads::new(&traces, &metadata, TraceExporterOutputFormat::V1, 512);
    assert!(matches!(
        planner.next(),
        Some(Payload::Oversized { index: 2 })
    ));
    let Some(Payload::Encoded { indices, .. }) = planner.next() else {
        panic!("expected V1 payload")
    };
    assert_eq!(indices, vec![0]);
    planner.fallback_to_v04();
    let mut seen = Vec::new();
    for payload in planner {
        let Payload::Encoded { indices, bytes } = payload else {
            panic!("unexpected fallback drop")
        };
        let selected: Vec<_> = indices.iter().map(|&index| traces[index].clone()).collect();
        assert_eq!(bytes, encoded(&selected, TraceExporterOutputFormat::V04));
        assert!(bytes.len() <= 512);
        seen.extend(indices);
    }
    assert_eq!(seen, vec![1, 3, 4]);
}
