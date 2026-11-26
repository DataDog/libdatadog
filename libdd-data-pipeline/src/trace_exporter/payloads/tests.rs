// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use super::super::trace_serializer::TraceSerializer;
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
    TraceSerializer::encode_range(traces, &TracerMetadata::default(), format, 0).unwrap()
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
        let mut planner = Payloads::new(&traces, &metadata, format, limit);
        while let Some(payload) = planner.next(&|| false) {
            let Payload::Encoded { indices, bytes } = payload else {
                panic!("unexpected drop");
            };
            assert!(bytes.len() <= limit);
            let selected: Vec<_> = traces[indices.clone()].to_vec();
            assert_eq!(bytes, encoded(&selected, format));
            seen.extend(indices);
        }
        assert_eq!(seen, (0..9).collect::<Vec<_>>());
    }
}

#[test]
fn isolates_oversized_chunks_without_losing_neighbours() {
    for format in [
        TraceExporterOutputFormat::V04,
        TraceExporterOutputFormat::V05,
        TraceExporterOutputFormat::V1,
    ] {
        let mut traces: Vec<_> = (0..8).map(|id| chunk(id, 8)).collect();
        traces[3] = chunk(3, 4096);
        let metadata = TracerMetadata::default();
        let mut requests = 0;
        let mut seen = Vec::new();
        let mut planner = Payloads::new(&traces, &metadata, format, 2048);
        while let Some(payload) = planner.next(&|| false) {
            match payload {
                Payload::Encoded { indices, .. } => {
                    requests += 1;
                    seen.extend(indices);
                }
                Payload::Oversized { index } => assert_eq!(index, 3),
                _ => panic!("unexpected outcome"),
            }
        }
        assert!(requests >= 2);
        assert_eq!(seen, (0..8).filter(|&i| i != 3).collect::<Vec<_>>());
    }
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
                matches!(payloads.next(&|| false).unwrap(), Payload::Encoded { .. }),
                limit >= size
            );
            assert!(payloads.next(&|| false).is_none());
        }
    }
}

#[test]
fn stops_between_chunks() {
    use std::sync::atomic::{AtomicUsize, Ordering};
    let traces: Vec<_> = (0..1000).map(|id| chunk(id, 8)).collect();
    let checks = AtomicUsize::new(0);
    let stop = || checks.fetch_add(1, Ordering::Relaxed) > 0;
    let metadata = TracerMetadata::default();
    let mut payloads = Payloads::new(&traces, &metadata, TraceExporterOutputFormat::V1, 1024);
    assert!(matches!(payloads.next(&stop), Some(Payload::Stopped)));
    assert!(checks.load(Ordering::Relaxed) < 20);
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
    let Payload::Encoded { bytes, .. } = payloads.next(&|| false).unwrap() else {
        panic!("expected a payload");
    };
    assert_eq!(bytes, expected);
}

#[test]
fn v04_packing_accounts_for_array_header_transitions() {
    for &count in if cfg!(miri) {
        &[15, 16][..]
    } else {
        &[15, 16, 65_535, 65_536][..]
    } {
        let survivors = vec![vec![SpanBytes::default()]; count];
        let expected = encoded(&survivors, TraceExporterOutputFormat::V04);
        let traces = survivors;
        let metadata = TracerMetadata::default();
        let mut payloads = Payloads::new(
            &traces,
            &metadata,
            TraceExporterOutputFormat::V04,
            expected.len(),
        );
        let Some(Payload::Encoded { indices, bytes }) = payloads.next(&|| false) else {
            panic!("expected packed survivors");
        };
        assert_eq!(indices.len(), count);
        assert_eq!(bytes, expected);
        assert!(payloads.next(&|| false).is_none());
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
        let mut planner = Payloads::new(&traces, &metadata, TraceExporterOutputFormat::V04, 256);
        while planner.next(&stop).is_some() {}
        stopped.store(true, Ordering::Relaxed);
        assert!(planner.next(&stop).is_none());
    }
}

#[test]
fn fallback_only_replans_unsent_survivors() {
    let mut traces: Vec<_> = (0..4).map(|id| chunk(id, 300)).collect();
    traces.insert(1, chunk(10, 2048));
    let metadata = TracerMetadata::default();
    let mut planner = Payloads::new(&traces, &metadata, TraceExporterOutputFormat::V1, 512);
    let Some(Payload::Encoded { indices, .. }) = planner.next(&|| false) else {
        panic!("expected V1 payload")
    };
    assert_eq!(indices, 0..1);
    planner.fallback_to_v04();
    let mut seen = Vec::new();
    while let Some(payload) = planner.next(&|| false) {
        if matches!(payload, Payload::Oversized { index: 1 }) {
            continue;
        }
        let Payload::Encoded { indices, bytes } = payload else {
            panic!("unexpected fallback drop")
        };
        let selected: Vec<_> = traces[indices.clone()].to_vec();
        assert_eq!(bytes, encoded(&selected, TraceExporterOutputFormat::V04));
        assert!(bytes.len() <= 512);
        seen.extend(indices);
    }
    assert_eq!(seen, vec![2, 3, 4]);
}

#[test]
fn empty_control_payloads_obey_the_limit() {
    let metadata = TracerMetadata {
        env: "x".repeat(2048),
        ..Default::default()
    };
    for format in [
        TraceExporterOutputFormat::V04,
        TraceExporterOutputFormat::V05,
        TraceExporterOutputFormat::V1,
    ] {
        let payload = empty(&metadata, format, 4096, &|| false).unwrap();
        let size = payload.len();
        assert_eq!(empty(&metadata, format, size, &|| false).unwrap(), payload);
        assert!(
            matches!(empty(&metadata, format, size - 1, &|| false), Err(TraceExporterError::Io(error)) if error.kind() == io::ErrorKind::FileTooLarge)
        );
        assert!(
            matches!(empty(&metadata, format, size, &|| true), Err(TraceExporterError::Io(error)) if error.kind() == io::ErrorKind::Interrupted)
        );
    }
}
