// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use crate::span_concentrator::aggregation::{OwnedAggregationKey, TRACER_BLOCKED_VALUE};

use super::*;
use libdd_trace_utils::span::v04::VecMap;
use libdd_trace_utils::span::{trace_utils::compute_top_level_span, v04::SpanSlice};
use rand::{thread_rng, Rng};

const BUCKET_SIZE: u64 = Duration::from_secs(2).as_nanos() as u64;

fn get_span_kinds() -> Vec<String> {
    vec![
        "client".to_string(),
        "server".to_string(),
        "consumer".to_string(),
        "producer".to_string(),
    ]
}

/// Return a random timestamp within the corresponding bucket (now - offset)
fn get_timestamp_in_bucket(aligned_now: u64, bucket_size: u64, offset: u64) -> u64 {
    aligned_now - bucket_size * offset + thread_rng().gen_range(0..BUCKET_SIZE)
}

/// Create a test span with given attributes
#[allow(clippy::too_many_arguments)]
fn get_test_span<'a>(
    now: SystemTime,
    span_id: u64,
    parent_id: u64,
    duration: i64,
    offset: u64,
    service: &'a str,
    resource: &'a str,
    error: i32,
) -> SpanSlice<'a> {
    let aligned_now = align_timestamp(
        system_time_to_unix_duration(now).as_nanos() as u64,
        BUCKET_SIZE,
    );
    SpanSlice {
        span_id,
        parent_id,
        duration,
        start: get_timestamp_in_bucket(aligned_now, BUCKET_SIZE, offset) as i64 - duration,
        service,
        name: "query",
        resource,
        error,
        r#type: "db",
        ..Default::default()
    }
}

#[allow(clippy::too_many_arguments)]
fn get_test_span_with_meta<'a>(
    now: SystemTime,
    span_id: u64,
    parent_id: u64,
    duration: i64,
    offset: u64,
    service: &'a str,
    resource: &'a str,
    error: i32,
    meta: &'a [(&str, &str)],
    metrics: &'a [(&str, f64)],
) -> SpanSlice<'a> {
    let mut span = get_test_span(
        now, span_id, parent_id, duration, offset, service, resource, error,
    );
    for (k, v) in meta {
        span.meta.insert(*k, *v);
    }
    span.metrics = VecMap::new();
    for (k, v) in metrics {
        span.metrics.insert(*k, *v);
    }
    span
}

fn assert_counts_equal(expected: Vec<pb::ClientGroupedStats>, actual: Vec<pb::ClientGroupedStats>) {
    let mut expected_map = HashMap::new();
    let mut actual_map = HashMap::new();
    expected.into_iter().for_each(|mut group| {
        group.ok_summary = vec![];
        group.error_summary = vec![];
        expected_map.insert(OwnedAggregationKey::from(group.clone()), group);
    });
    actual.into_iter().for_each(|mut group| {
        group.ok_summary = vec![];
        group.error_summary = vec![];
        actual_map.insert(OwnedAggregationKey::from(group.clone()), group);
    });
    assert_eq!(expected_map, actual_map)
}

/// Test that the SpanConcentrator does not create buckets older than the exporter initialization
#[test]
fn test_concentrator_oldest_timestamp_cold() {
    let now = SystemTime::now();
    let mut concentrator = SpanConcentrator::new(
        Duration::from_nanos(BUCKET_SIZE),
        now,
        vec![],
        vec![],
        None,
        #[cfg(feature = "stats-obfuscation")]
        None,
    );
    let mut spans = vec![
        get_test_span(now, 1, 0, 50, 5, "A1", "resource1", 0),
        get_test_span(now, 1, 0, 40, 4, "A1", "resource1", 0),
        get_test_span(now, 1, 0, 30, 3, "A1", "resource1", 0),
        get_test_span(now, 1, 0, 20, 2, "A1", "resource1", 0),
        get_test_span(now, 1, 0, 10, 1, "A1", "resource1", 0),
        get_test_span(now, 1, 0, 1, 0, "A1", "resource1", 0),
    ];
    compute_top_level_span(spans.as_mut_slice());
    for span in &spans {
        concentrator.add_span(span);
    }

    let mut flushtime = now;

    // Assert we didn't insert spans in older buckets
    for _ in 0..concentrator.buffer_len {
        let stats = concentrator.flush(flushtime, false).all_buckets();
        assert_eq!(stats.len(), 0, "We should get 0 time buckets");
        flushtime += Duration::from_nanos(concentrator.bucket_size);
    }

    let stats = concentrator.flush(flushtime, false).all_buckets();

    assert_eq!(stats.len(), 1, "We should get exactly one time bucket");

    // First oldest bucket aggregates old past time buckets, so each count
    // should be an aggregated total across the spans.
    let expected = vec![pb::ClientGroupedStats {
        service: "A1".to_string(),
        resource: "resource1".to_string(),
        r#type: "db".to_string(),
        name: "query".to_string(),
        duration: 151,
        hits: 6,
        top_level_hits: 6,
        errors: 0,
        is_trace_root: pb::Trilean::True.into(),
        ..Default::default()
    }];
    assert_counts_equal(expected, stats.first().unwrap().stats.clone());
}

/// Test that the SpanConcentrator does not create buckets older than the exporter initialization
/// with multiple active buckets
#[test]
fn test_concentrator_oldest_timestamp_hot() {
    let now = SystemTime::now();
    let mut concentrator = SpanConcentrator::new(
        Duration::from_nanos(BUCKET_SIZE),
        now,
        vec![],
        vec![],
        None,
        #[cfg(feature = "stats-obfuscation")]
        None,
    );
    let mut spans = vec![
        get_test_span(now, 1, 0, 50, 5, "A1", "resource1", 0),
        get_test_span(now, 1, 0, 40, 4, "A1", "resource1", 0),
        get_test_span(now, 1, 0, 30, 3, "A1", "resource1", 0),
        get_test_span(now, 1, 0, 20, 2, "A1", "resource1", 0),
        get_test_span(now, 1, 0, 10, 1, "A1", "resource1", 0),
        get_test_span(now, 1, 0, 1, 0, "A1", "resource1", 0),
    ];
    compute_top_level_span(spans.as_mut_slice());

    let mut flushtime = now;
    concentrator.oldest_timestamp = align_timestamp(
        system_time_to_unix_duration(flushtime).as_nanos() as u64,
        concentrator.bucket_size,
    ) - (concentrator.buffer_len as u64 - 1)
        * concentrator.bucket_size;

    for span in &spans {
        concentrator.add_span(span);
    }

    for _ in 0..(concentrator.buffer_len - 1) {
        let stats = concentrator.flush(flushtime, false).all_buckets();
        assert!(stats.is_empty(), "We should get 0 time buckets");
        flushtime += Duration::from_nanos(concentrator.bucket_size);
    }

    let stats = concentrator.flush(flushtime, false).all_buckets();
    assert_eq!(stats.len(), 1, "We should get exactly one time bucket");

    // First oldest bucket aggregates, it should have it all except the
    // last four spans that have offset of 0.
    let expected = vec![pb::ClientGroupedStats {
        service: "A1".to_string(),
        resource: "resource1".to_string(),
        r#type: "db".to_string(),
        name: "query".to_string(),
        duration: 150,
        hits: 5,
        top_level_hits: 5,
        errors: 0,
        is_trace_root: pb::Trilean::True.into(),
        ..Default::default()
    }];
    assert_counts_equal(expected, stats.first().unwrap().stats.clone());

    flushtime += Duration::from_nanos(concentrator.bucket_size);
    let stats = concentrator.flush(flushtime, false).all_buckets();
    assert_eq!(stats.len(), 1, "We should get exactly one time bucket");

    // Stats of the last four spans.
    let expected = vec![pb::ClientGroupedStats {
        service: "A1".to_string(),
        resource: "resource1".to_string(),
        r#type: "db".to_string(),
        name: "query".to_string(),
        duration: 1,
        hits: 1,
        top_level_hits: 1,
        errors: 0,
        is_trace_root: pb::Trilean::True.into(),
        ..Default::default()
    }];
    assert_counts_equal(expected, stats.first().unwrap().stats.clone());
}

/// Tests that the total stats are correct, independently of the
/// time bucket they end up.
#[test]
fn test_concentrator_stats_totals() {
    let now = SystemTime::now();
    let mut concentrator = SpanConcentrator::new(
        Duration::from_nanos(BUCKET_SIZE),
        now,
        vec![],
        vec![],
        None,
        #[cfg(feature = "stats-obfuscation")]
        None,
    );
    let aligned_now = align_timestamp(
        system_time_to_unix_duration(now).as_nanos() as u64,
        concentrator.bucket_size,
    );

    // update oldest_timestamp as if it is running for quite some time, to avoid the fact that
    // at startup it only allows recent stats.
    concentrator.oldest_timestamp =
        aligned_now - concentrator.buffer_len as u64 * concentrator.bucket_size;

    let mut spans = vec![
        get_test_span(now, 1, 0, 50, 5, "A1", "resource1", 0),
        get_test_span(now, 1, 0, 40, 4, "A1", "resource1", 0),
        get_test_span(now, 1, 0, 30, 3, "A1", "resource1", 0),
        get_test_span(now, 1, 0, 20, 2, "A1", "resource1", 0),
        get_test_span(now, 1, 0, 10, 1, "A1", "resource1", 0),
        get_test_span(now, 1, 0, 1, 0, "A1", "resource1", 0),
    ];
    compute_top_level_span(spans.as_mut_slice());

    let mut total_duration = 0;
    let mut total_hits = 0;
    let mut total_errors = 0;
    let mut total_top_level_hits = 0;

    for span in &spans {
        concentrator.add_span(span);
    }

    let mut flushtime = now;

    for _ in 0..=concentrator.buffer_len {
        let stats = concentrator.flush(flushtime, false).all_buckets();
        if stats.is_empty() {
            continue;
        }

        for group in &stats.first().unwrap().stats {
            total_duration += group.duration;
            total_hits += group.hits;
            total_errors += group.errors;
            total_top_level_hits += group.top_level_hits;
        }

        flushtime += Duration::from_nanos(concentrator.bucket_size);
    }

    assert_eq!(total_duration, 50 + 40 + 30 + 20 + 10 + 1);
    assert_eq!(total_hits, spans.len() as u64);
    assert_eq!(total_top_level_hits, spans.len() as u64);
    assert_eq!(total_errors, 0);
}

#[test]
/// Tests exhaustively each stats bucket, over multiple time
/// buckets.
fn test_concentrator_stats_counts() {
    let now = SystemTime::now();
    let mut concentrator = SpanConcentrator::new(
        Duration::from_nanos(BUCKET_SIZE),
        now,
        vec![],
        vec![],
        None,
        #[cfg(feature = "stats-obfuscation")]
        None,
    );
    let aligned_now = align_timestamp(
        system_time_to_unix_duration(now).as_nanos() as u64,
        concentrator.bucket_size,
    );

    // update oldest_timestamp as if it is running for quite some time, to avoid the fact that
    // at startup it only allows recent stats.
    concentrator.oldest_timestamp =
        aligned_now - concentrator.buffer_len as u64 * concentrator.bucket_size;

    // Build a trace with stats which should cover 3 time buckets.
    let mut spans = vec![
        // more than 2 buckets old, should be added to the 2 bucket-old, first flush.
        get_test_span(now, 1, 0, 111, 10, "A1", "resource1", 0),
        get_test_span(now, 1, 0, 222, 3, "A1", "resource1", 0),
        get_test_span_with_meta(
            now,
            11,
            0,
            333,
            3,
            "A1",
            "resource3",
            0,
            &[("span.kind", "client")],
            &[],
        ),
        get_test_span_with_meta(
            now,
            12,
            0,
            444,
            3,
            "A1",
            "resource3",
            0,
            &[("span.kind", "server")],
            &[],
        ),
        // 2 buckets old, part of the first flush
        get_test_span(now, 1, 0, 24, 2, "A1", "resource1", 0),
        get_test_span(now, 2, 0, 12, 2, "A1", "resource1", 2),
        get_test_span(now, 3, 0, 40, 2, "A2", "resource2", 2),
        get_test_span(now, 4, 0, 300000000000, 2, "A2", "resource2", 2), // 5 minutes trace
        get_test_span(now, 5, 0, 30, 2, "A2", "resourcefoo", 0),
        // 1 bucket old, part of the second flush
        get_test_span(now, 6, 0, 24, 1, "A1", "resource2", 0),
        get_test_span(now, 7, 0, 12, 1, "A1", "resource1", 2),
        get_test_span(now, 8, 0, 40, 1, "A2", "resource1", 2),
        get_test_span(now, 9, 0, 30, 1, "A2", "resource2", 2),
        get_test_span(now, 10, 0, 3600000000000, 1, "A2", "resourcefoo", 0), // 1 hour trace
        // present data, part of the third flush
        get_test_span(now, 6, 0, 24, 0, "A1", "resource2", 0),
    ];
    let mut expected_counts_by_time = HashMap::new();
    expected_counts_by_time.insert(
        aligned_now - 2 * BUCKET_SIZE,
        vec![
            pb::ClientGroupedStats {
                service: "A1".to_string(),
                resource: "resource1".to_string(),
                r#type: "db".to_string(),
                name: "query".to_string(),
                duration: 369,
                hits: 4,
                top_level_hits: 4,
                errors: 1,
                is_trace_root: pb::Trilean::True.into(),
                ..Default::default()
            },
            pb::ClientGroupedStats {
                service: "A2".to_string(),
                resource: "resource2".to_string(),
                r#type: "db".to_string(),
                name: "query".to_string(),
                duration: 300000000040,
                hits: 2,
                top_level_hits: 2,
                errors: 2,
                is_trace_root: pb::Trilean::True.into(),
                ..Default::default()
            },
            pb::ClientGroupedStats {
                service: "A2".to_string(),
                resource: "resourcefoo".to_string(),
                r#type: "db".to_string(),
                name: "query".to_string(),
                duration: 30,
                hits: 1,
                top_level_hits: 1,
                errors: 0,
                is_trace_root: pb::Trilean::True.into(),
                ..Default::default()
            },
            pb::ClientGroupedStats {
                service: "A1".to_string(),
                resource: "resource3".to_string(),
                r#type: "db".to_string(),
                name: "query".to_string(),
                span_kind: "client".to_string(),
                duration: 333,
                hits: 1,
                top_level_hits: 1,
                errors: 0,
                is_trace_root: pb::Trilean::True.into(),
                ..Default::default()
            },
            pb::ClientGroupedStats {
                service: "A1".to_string(),
                resource: "resource3".to_string(),
                r#type: "db".to_string(),
                name: "query".to_string(),
                span_kind: "server".to_string(),
                duration: 444,
                hits: 1,
                top_level_hits: 1,
                errors: 0,
                is_trace_root: pb::Trilean::True.into(),
                ..Default::default()
            },
        ],
    );
    // 1-bucket old flush
    expected_counts_by_time.insert(
        aligned_now - BUCKET_SIZE,
        vec![
            pb::ClientGroupedStats {
                service: "A1".to_string(),
                resource: "resource1".to_string(),
                r#type: "db".to_string(),
                name: "query".to_string(),
                duration: 12,
                hits: 1,
                top_level_hits: 1,
                errors: 1,
                is_trace_root: pb::Trilean::True.into(),
                ..Default::default()
            },
            pb::ClientGroupedStats {
                service: "A1".to_string(),
                resource: "resource2".to_string(),
                r#type: "db".to_string(),
                name: "query".to_string(),
                duration: 24,
                hits: 1,
                top_level_hits: 1,
                errors: 0,
                is_trace_root: pb::Trilean::True.into(),
                ..Default::default()
            },
            pb::ClientGroupedStats {
                service: "A2".to_string(),
                resource: "resource1".to_string(),
                r#type: "db".to_string(),
                name: "query".to_string(),
                duration: 40,
                hits: 1,
                top_level_hits: 1,
                errors: 1,
                is_trace_root: pb::Trilean::True.into(),
                ..Default::default()
            },
            pb::ClientGroupedStats {
                service: "A2".to_string(),
                resource: "resource2".to_string(),
                r#type: "db".to_string(),
                name: "query".to_string(),
                duration: 30,
                hits: 1,
                top_level_hits: 1,
                errors: 1,
                is_trace_root: pb::Trilean::True.into(),
                ..Default::default()
            },
            pb::ClientGroupedStats {
                service: "A2".to_string(),
                resource: "resourcefoo".to_string(),
                r#type: "db".to_string(),
                name: "query".to_string(),
                duration: 3600000000000,
                hits: 1,
                top_level_hits: 1,
                errors: 0,
                is_trace_root: pb::Trilean::True.into(),
                ..Default::default()
            },
        ],
    );
    // last bucket to be flushed
    expected_counts_by_time.insert(
        aligned_now,
        vec![pb::ClientGroupedStats {
            service: "A1".to_string(),
            resource: "resource2".to_string(),
            r#type: "db".to_string(),
            name: "query".to_string(),
            duration: 24,
            hits: 1,
            top_level_hits: 1,
            errors: 0,
            is_trace_root: pb::Trilean::True.into(),
            ..Default::default()
        }],
    );
    expected_counts_by_time.insert(aligned_now + BUCKET_SIZE, vec![]);

    compute_top_level_span(spans.as_mut_slice());

    for span in &spans {
        concentrator.add_span(span);
    }

    let mut flushtime = now;

    for _ in 0..=concentrator.buffer_len + 2 {
        let stats = concentrator.flush(flushtime, false).all_buckets();
        let expected_flushed_timestamps = align_timestamp(
            system_time_to_unix_duration(flushtime).as_nanos() as u64,
            concentrator.bucket_size,
        ) - concentrator.buffer_len as u64
            * concentrator.bucket_size;
        if expected_counts_by_time
            .get(&expected_flushed_timestamps)
            .expect("Unexpected flushed timestamps")
            .is_empty()
        {
            // That's a flush for which we expect no data
            continue;
        }

        assert_eq!(stats.len(), 1, "We should get exactly one bucket");
        assert_eq!(expected_flushed_timestamps, stats.first().unwrap().start);
        assert_counts_equal(
            expected_counts_by_time
                .get(&expected_flushed_timestamps)
                .unwrap()
                .clone(),
            stats.first().unwrap().stats.clone(),
        );

        let stats = concentrator.flush(flushtime, false).all_buckets();
        assert_eq!(
            stats.len(),
            0,
            "Second flush on the same time should be empty"
        );
        flushtime += Duration::from_nanos(concentrator.bucket_size);
    }
}

/// Test the criteria to include a span in stats computation
#[test]
fn test_span_should_be_included_in_stats() {
    let now = SystemTime::now();
    let mut spans = vec![
        // root span is included
        get_test_span(now, 1, 0, 40, 10, "A1", "resource1", 0),
        // non top level span is not included
        get_test_span(now, 2, 1, 30, 10, "A1", "resource1", 0),
        // non-root, non-top level, but eligible span.kind is included
        get_test_span_with_meta(
            now,
            3,
            2,
            20,
            10,
            "A1",
            "resource1",
            0,
            &[("span.kind", "client")],
            &[],
        ),
        // non-root but top level span is included
        get_test_span(now, 4, 1000, 10, 10, "A1", "resource1", 0),
        // non-root, non-top level, but measured span is included
        get_test_span_with_meta(
            now,
            5,
            1,
            5,
            10,
            "A1",
            "resource1",
            0,
            &[],
            &[("_dd.measured", 1.0)],
        ),
    ];
    compute_top_level_span(spans.as_mut_slice());
    let mut concentrator = SpanConcentrator::new(
        Duration::from_nanos(BUCKET_SIZE),
        now,
        get_span_kinds(),
        vec![],
        None,
        #[cfg(feature = "stats-obfuscation")]
        None,
    );
    for span in &spans {
        concentrator.add_span(span);
    }

    let expected = vec![
        // contains only the root span
        pb::ClientGroupedStats {
            service: "A1".to_string(),
            resource: "resource1".to_string(),
            r#type: "db".to_string(),
            name: "query".to_string(),
            duration: 40,
            hits: 1,
            top_level_hits: 1,
            errors: 0,
            is_trace_root: pb::Trilean::True.into(),
            ..Default::default()
        },
        // contains the top-level span and the measured span
        pb::ClientGroupedStats {
            service: "A1".to_string(),
            resource: "resource1".to_string(),
            r#type: "db".to_string(),
            name: "query".to_string(),
            duration: 15,
            hits: 2,
            top_level_hits: 1,
            errors: 0,
            is_trace_root: pb::Trilean::False.into(),
            ..Default::default()
        },
        // contains only the client span
        pb::ClientGroupedStats {
            service: "A1".to_string(),
            resource: "resource1".to_string(),
            r#type: "db".to_string(),
            name: "query".to_string(),
            duration: 20,
            hits: 1,
            top_level_hits: 0,
            errors: 0,
            is_trace_root: pb::Trilean::False.into(),
            span_kind: "client".to_string(),
            ..Default::default()
        },
    ];

    let stats = concentrator
        .flush(
            now + Duration::from_nanos(concentrator.bucket_size * concentrator.buffer_len as u64),
            false,
        )
        .all_buckets();
    assert_counts_equal(
        expected,
        stats
            .first()
            .expect("There should be at least one time bucket")
            .stats
            .clone(),
    );
}

/// Test that partial spans are ignored for stats
#[test]
fn test_ignore_partial_spans() {
    let now = SystemTime::now();
    let mut spans = vec![get_test_span(now, 1, 0, 50, 5, "A1", "resource1", 0)];
    spans
        .get_mut(0)
        .unwrap()
        .metrics
        .insert("_dd.partial_version", 830604.0);
    compute_top_level_span(spans.as_mut_slice());
    let mut concentrator = SpanConcentrator::new(
        Duration::from_nanos(BUCKET_SIZE),
        now,
        get_span_kinds(),
        vec![],
        None,
        #[cfg(feature = "stats-obfuscation")]
        None,
    );
    for span in &spans {
        concentrator.add_span(span);
    }

    let stats = concentrator
        .flush(
            now + Duration::from_nanos(concentrator.bucket_size * concentrator.buffer_len as u64),
            false,
        )
        .all_buckets();
    assert_eq!(0, stats.len());
}

/// Test the force flush parameter
#[test]
fn test_force_flush() {
    let now = SystemTime::now();
    let mut spans = vec![get_test_span(now, 1, 0, 50, 5, "A1", "resource1", 0)];
    compute_top_level_span(spans.as_mut_slice());
    let mut concentrator = SpanConcentrator::new(
        Duration::from_nanos(BUCKET_SIZE),
        now,
        get_span_kinds(),
        vec![],
        None,
        #[cfg(feature = "stats-obfuscation")]
        None,
    );
    for span in &spans {
        concentrator.add_span(span);
    }

    // flushtime is 1h before now to make sure the bucket is not old enough to be flushed
    // without force flush
    let flushtime = now - Duration::from_secs(3600);

    // Bucket should not be flushed without force flush
    let stats = concentrator.flush(flushtime, false).all_buckets();
    assert_eq!(0, stats.len());

    let stats = concentrator.flush(flushtime, true).all_buckets();
    assert_eq!(1, stats.len());
}

/// Test the peer tags aggregation
#[test]
fn test_peer_tags_aggregation() {
    let now = SystemTime::now();
    let mut spans = vec![
        get_test_span(now, 1, 0, 100, 5, "A1", "GET /users", 0),
        get_test_span_with_meta(
            now,
            2,
            1,
            75,
            5,
            "A1",
            "SELECT user_id from users WHERE user_name = ?",
            0,
            &[
                ("span.kind", "client"),
                ("db.instance", "i-1234"),
                ("db.system", "postgres"),
                ("region", "us1"),
            ],
            &[("_dd.measured", 1.0)],
        ),
        get_test_span_with_meta(
            now,
            3,
            1,
            75,
            5,
            "A1",
            "SELECT user_id from users WHERE user_name = ?",
            0,
            &[
                ("span.kind", "client"),
                ("db.instance", "i-1234"),
                ("db.system", "postgres"),
                ("region", "us1"),
            ],
            &[("_dd.measured", 1.0)],
        ),
        get_test_span_with_meta(
            now,
            4,
            1,
            50,
            5,
            "A1",
            "SELECT user_id from users WHERE user_name = ?",
            0,
            &[
                ("span.kind", "client"),
                ("db.instance", "i-4321"),
                ("db.system", "postgres"),
                ("region", "us1"),
            ],
            &[("_dd.measured", 1.0)],
        ),
    ];
    compute_top_level_span(spans.as_mut_slice());
    let mut concentrator_without_peer_tags = SpanConcentrator::new(
        Duration::from_nanos(BUCKET_SIZE),
        now,
        get_span_kinds(),
        vec![],
        None,
        #[cfg(feature = "stats-obfuscation")]
        None,
    );
    let mut concentrator_with_peer_tags = SpanConcentrator::new(
        Duration::from_nanos(BUCKET_SIZE),
        now,
        get_span_kinds(),
        vec!["db.instance".to_string(), "db.system".to_string()],
        None,
        #[cfg(feature = "stats-obfuscation")]
        None,
    );
    for span in &spans {
        concentrator_without_peer_tags.add_span(span);
    }
    for span in &spans {
        concentrator_with_peer_tags.add_span(span);
    }

    let flushtime = now
        + Duration::from_nanos(
            concentrator_with_peer_tags.bucket_size * concentrator_with_peer_tags.buffer_len as u64,
        );

    let expected_with_peer_tags = vec![
        pb::ClientGroupedStats {
            service: "A1".to_string(),
            resource: "GET /users".to_string(),
            r#type: "db".to_string(),
            name: "query".to_string(),
            duration: 100,
            hits: 1,
            top_level_hits: 1,
            errors: 0,
            is_trace_root: pb::Trilean::True.into(),
            ..Default::default()
        },
        pb::ClientGroupedStats {
            service: "A1".to_string(),
            resource: "SELECT user_id from users WHERE user_name = ?".to_string(),
            r#type: "db".to_string(),
            name: "query".to_string(),
            span_kind: "client".to_string(),
            peer_tags: vec![
                "db.instance:i-1234".to_string(),
                "db.system:postgres".to_string(),
            ],
            duration: 150,
            hits: 2,
            top_level_hits: 0,
            errors: 0,
            is_trace_root: pb::Trilean::False.into(),
            ..Default::default()
        },
        pb::ClientGroupedStats {
            service: "A1".to_string(),
            resource: "SELECT user_id from users WHERE user_name = ?".to_string(),
            r#type: "db".to_string(),
            name: "query".to_string(),
            span_kind: "client".to_string(),
            peer_tags: vec![
                "db.instance:i-4321".to_string(),
                "db.system:postgres".to_string(),
            ],
            duration: 50,
            hits: 1,
            top_level_hits: 0,
            errors: 0,
            is_trace_root: pb::Trilean::False.into(),
            ..Default::default()
        },
    ];

    let expected_without_peer_tags = vec![
        pb::ClientGroupedStats {
            service: "A1".to_string(),
            resource: "GET /users".to_string(),
            r#type: "db".to_string(),
            name: "query".to_string(),
            duration: 100,
            hits: 1,
            top_level_hits: 1,
            errors: 0,
            is_trace_root: pb::Trilean::True.into(),
            ..Default::default()
        },
        pb::ClientGroupedStats {
            service: "A1".to_string(),
            resource: "SELECT user_id from users WHERE user_name = ?".to_string(),
            r#type: "db".to_string(),
            name: "query".to_string(),
            span_kind: "client".to_string(),
            duration: 200,
            hits: 3,
            top_level_hits: 0,
            errors: 0,
            is_trace_root: pb::Trilean::False.into(),
            ..Default::default()
        },
    ];

    let stats_with_peer_tags = concentrator_with_peer_tags
        .flush(flushtime, false)
        .all_buckets();
    assert_counts_equal(
        expected_with_peer_tags,
        stats_with_peer_tags
            .first()
            .expect("There should be at least one time bucket")
            .stats
            .clone(),
    );

    let stats_without_peer_tags = concentrator_without_peer_tags
        .flush(flushtime, false)
        .all_buckets();
    assert_counts_equal(
        expected_without_peer_tags,
        stats_without_peer_tags
            .first()
            .expect("There should be at least one time bucket")
            .stats
            .clone(),
    );
}

/// Test that spans differing only by peer-tag IPs aggregate after IP quantization
#[test]
fn test_peer_tags_quantization_aggregation() {
    let now = SystemTime::now();
    let mut spans = vec![
        get_test_span_with_meta(
            now,
            2,
            1,
            75,
            5,
            "A1",
            "SELECT user_id from users WHERE user_name = ?",
            0,
            &[
                ("span.kind", "client"),
                ("db.instance", "i-1234"),
                ("db.system", "postgres"),
                ("region", "us1"),
                ("peer.hostname", "10.1.2.3"),
            ],
            &[("_dd.measured", 1.0)],
        ),
        get_test_span_with_meta(
            now,
            3,
            1,
            75,
            5,
            "A1",
            "SELECT user_id from users WHERE user_name = ?",
            0,
            &[
                ("span.kind", "client"),
                ("db.instance", "i-1234"),
                ("db.system", "postgres"),
                ("region", "us1"),
                ("peer.hostname", "10.1.2.4"),
            ],
            &[("_dd.measured", 1.0)],
        ),
        get_test_span_with_meta(
            now,
            4,
            1,
            75,
            5,
            "A1",
            "SELECT user_id from users WHERE user_name = ?",
            0,
            &[
                ("span.kind", "client"),
                ("db.instance", "i-1234"),
                ("db.system", "postgres"),
                ("region", "us1"),
                ("peer.hostname", "2001:db8:3333:4444:CCCC:DDDD:EEEE:FFFF"),
            ],
            &[("_dd.measured", 1.0)],
        ),
    ];
    compute_top_level_span(spans.as_mut_slice());
    let mut concentrator_with_peer_tags = SpanConcentrator::new(
        Duration::from_nanos(BUCKET_SIZE),
        now,
        get_span_kinds(),
        vec![
            "db.instance".to_string(),
            "db.system".to_string(),
            "peer.hostname".to_string(),
        ],
        None,
        #[cfg(feature = "stats-obfuscation")]
        None,
    );
    for span in &spans {
        concentrator_with_peer_tags.add_span(span);
    }

    let flushtime = now
        + Duration::from_nanos(
            concentrator_with_peer_tags.bucket_size * concentrator_with_peer_tags.buffer_len as u64,
        );

    let expected_with_peer_tags = vec![pb::ClientGroupedStats {
        service: "A1".to_string(),
        resource: "SELECT user_id from users WHERE user_name = ?".to_string(),
        r#type: "db".to_string(),
        name: "query".to_string(),
        duration: 225,
        hits: 3,
        top_level_hits: 3,
        errors: 0,
        is_trace_root: pb::Trilean::False.into(),
        span_kind: "client".to_string(),
        peer_tags: vec![
            "db.instance:i-1234".to_string(),
            "db.system:postgres".to_string(),
            "peer.hostname:blocked-ip-address".to_string(),
        ],
        ..Default::default()
    }];

    let stats_with_peer_tags = concentrator_with_peer_tags
        .flush(flushtime, false)
        .all_buckets();
    assert_counts_equal(
        expected_with_peer_tags,
        stats_with_peer_tags
            .first()
            .expect("There should be at least one time bucket")
            .stats
            .clone(),
    );
}

/// Test that internal spans with _dd.base_service use it as their sole peer tag
#[test]
fn test_base_service_peer_tag() {
    let now = SystemTime::now();
    let mut spans = vec![
        // Regular internal span without base_service (no peer tags)
        get_test_span_with_meta(
            now,
            1,
            0,
            100,
            5,
            "A1",
            "internal.operation",
            0,
            &[],
            &[("_dd.measured", 1.0)],
        ),
        // Internal span with _dd.base_service (should have base_service as peer tag)
        get_test_span_with_meta(
            now,
            2,
            0,
            75,
            5,
            "A1",
            "internal.with.base.service",
            0,
            &[("_dd.base_service", "original-service")],
            &[("_dd.measured", 1.0)],
        ),
        // Another internal span with same _dd.base_service (should aggregate together)
        get_test_span_with_meta(
            now,
            3,
            0,
            50,
            5,
            "A1",
            "internal.with.base.service",
            0,
            &[("_dd.base_service", "original-service")],
            &[("_dd.measured", 1.0)],
        ),
        // Internal span with different _dd.base_service (should be separate group)
        get_test_span_with_meta(
            now,
            4,
            0,
            60,
            5,
            "A1",
            "internal.with.base.service",
            0,
            &[("_dd.base_service", "other-service")],
            &[("_dd.measured", 1.0)],
        ),
        // Client span with _dd.base_service and other peer tags enabled
        // (should use configured peer tags, not base_service)
        get_test_span_with_meta(
            now,
            5,
            0,
            80,
            5,
            "A1",
            "SELECT * FROM users",
            0,
            &[
                ("span.kind", "client"),
                ("_dd.base_service", "ignored-for-client"),
                ("db.instance", "i-1234"),
                ("db.system", "postgres"),
            ],
            &[("_dd.measured", 1.0)],
        ),
    ];
    compute_top_level_span(spans.as_mut_slice());

    let mut concentrator = SpanConcentrator::new(
        Duration::from_nanos(BUCKET_SIZE),
        now,
        get_span_kinds(),
        vec!["db.instance".to_string(), "db.system".to_string()],
        None,
        #[cfg(feature = "stats-obfuscation")]
        None,
    );

    for span in &spans {
        concentrator.add_span(span);
    }

    let flushtime =
        now + Duration::from_nanos(concentrator.bucket_size * concentrator.buffer_len as u64);

    let expected = vec![
        // Internal span without base_service - no peer tags
        pb::ClientGroupedStats {
            service: "A1".to_string(),
            resource: "internal.operation".to_string(),
            r#type: "db".to_string(),
            name: "query".to_string(),
            duration: 100,
            hits: 1,
            top_level_hits: 1,
            errors: 0,
            is_trace_root: pb::Trilean::True.into(),
            ..Default::default()
        },
        // Internal spans with _dd.base_service="original-service" - aggregated with base_service
        // peer tag
        pb::ClientGroupedStats {
            service: "A1".to_string(),
            resource: "internal.with.base.service".to_string(),
            r#type: "db".to_string(),
            name: "query".to_string(),
            peer_tags: vec!["_dd.base_service:original-service".to_string()],
            duration: 125,
            hits: 2,
            top_level_hits: 2,
            errors: 0,
            is_trace_root: pb::Trilean::True.into(),
            ..Default::default()
        },
        // Internal span with _dd.base_service="other-service" - separate group
        pb::ClientGroupedStats {
            service: "A1".to_string(),
            resource: "internal.with.base.service".to_string(),
            r#type: "db".to_string(),
            name: "query".to_string(),
            peer_tags: vec!["_dd.base_service:other-service".to_string()],
            duration: 60,
            hits: 1,
            top_level_hits: 1,
            errors: 0,
            is_trace_root: pb::Trilean::True.into(),
            ..Default::default()
        },
        // Client span - uses configured peer tags, not base_service
        pb::ClientGroupedStats {
            service: "A1".to_string(),
            resource: "SELECT * FROM users".to_string(),
            r#type: "db".to_string(),
            name: "query".to_string(),
            span_kind: "client".to_string(),
            peer_tags: vec![
                "db.instance:i-1234".to_string(),
                "db.system:postgres".to_string(),
            ],
            duration: 80,
            hits: 1,
            top_level_hits: 1,
            errors: 0,
            is_trace_root: pb::Trilean::True.into(),
            ..Default::default()
        },
    ];

    let stats = concentrator.flush(flushtime, false).all_buckets();
    assert_counts_equal(
        expected,
        stats
            .first()
            .expect("There should be at least one time bucket")
            .stats
            .clone(),
    );
}

#[test]
fn test_compute_stats_for_span_kind() {
    let test_cases: Vec<(SpanSlice, bool)> = vec![
        (
            SpanSlice {
                meta: vec![("span.kind", "server")].into(),
                ..Default::default()
            },
            true,
        ),
        (
            SpanSlice {
                meta: vec![("span.kind", "consumer")].into(),
                ..Default::default()
            },
            true,
        ),
        (
            SpanSlice {
                meta: vec![("span.kind", "client")].into(),
                ..Default::default()
            },
            true,
        ),
        (
            SpanSlice {
                meta: vec![("span.kind", "producer")].into(),
                ..Default::default()
            },
            true,
        ),
        (
            SpanSlice {
                meta: vec![("span.kind", "internal")].into(),
                ..Default::default()
            },
            false,
        ),
        (
            SpanSlice {
                meta: vec![("span.kind", "SERVER")].into(),
                ..Default::default()
            },
            true,
        ),
        (
            SpanSlice {
                meta: vec![("span.kind", "CONSUMER")].into(),
                ..Default::default()
            },
            true,
        ),
        (
            SpanSlice {
                meta: vec![("span.kind", "CLIENT")].into(),
                ..Default::default()
            },
            true,
        ),
        (
            SpanSlice {
                meta: vec![("span.kind", "PRODUCER")].into(),
                ..Default::default()
            },
            true,
        ),
        (
            SpanSlice {
                meta: vec![("span.kind", "INTERNAL")].into(),
                ..Default::default()
            },
            false,
        ),
        (
            SpanSlice {
                meta: vec![("span.kind", "SerVER")].into(),
                ..Default::default()
            },
            true,
        ),
        (
            SpanSlice {
                meta: vec![("span.kind", "ConSUMeR")].into(),
                ..Default::default()
            },
            true,
        ),
        (
            SpanSlice {
                meta: vec![("span.kind", "CLiENT")].into(),
                ..Default::default()
            },
            true,
        ),
        (
            SpanSlice {
                meta: vec![("span.kind", "PROducER")].into(),
                ..Default::default()
            },
            true,
        ),
        (
            SpanSlice {
                meta: vec![("span.kind", "INtERNAL")].into(),
                ..Default::default()
            },
            false,
        ),
        (
            SpanSlice {
                meta: vec![("span.kind", "")].into(),
                ..Default::default()
            },
            false,
        ),
        (
            SpanSlice {
                meta: vec![].into(),
                ..Default::default()
            },
            false,
        ),
    ];

    for (span, is_eligible) in test_cases {
        assert!(is_span_eligible(&span, &get_span_kinds()) == is_eligible)
    }
}

#[test]
fn test_pb_span() {
    let now = SystemTime::now();
    let mut concentrator = SpanConcentrator::new(
        Duration::from_nanos(BUCKET_SIZE),
        now,
        get_span_kinds(),
        vec!["db.instance".to_string(), "db.system".to_string()],
        None,
        #[cfg(feature = "stats-obfuscation")]
        None,
    );
    let aligned_now = align_timestamp(
        system_time_to_unix_duration(now).as_nanos() as u64,
        concentrator.bucket_size,
    );

    let mut pb_spans = vec![
        // Root span
        pb::Span {
            service: "service1".to_string(),
            name: "query".to_string(),
            resource: "GET /users".to_string(),
            trace_id: 1,
            span_id: 1,
            parent_id: 0,
            start: (aligned_now - BUCKET_SIZE) as i64,
            duration: 100,
            error: 0,
            r#type: "db".to_string(),
            meta: std::collections::HashMap::new(),
            metrics: std::collections::HashMap::new(),
            meta_struct: std::collections::HashMap::new(),
            span_links: vec![],
            span_events: vec![],
        },
        // Child span not measured
        pb::Span {
            service: "service1".to_string(),
            name: "query".to_string(),
            resource: "GET /users".to_string(),
            trace_id: 1,
            span_id: 2,
            parent_id: 1,
            start: (aligned_now - BUCKET_SIZE + 10) as i64,
            duration: 50,
            error: 0,
            r#type: "db".to_string(),
            meta: std::collections::HashMap::new(),
            metrics: std::collections::HashMap::new(),
            meta_struct: std::collections::HashMap::new(),
            span_links: vec![],
            span_events: vec![],
        },
        // Span with span.kind = client and peer tags
        {
            let mut meta = std::collections::HashMap::new();
            meta.insert("span.kind".to_string(), "client".to_string());
            meta.insert("db.instance".to_string(), "i-1234".to_string());
            meta.insert("db.system".to_string(), "postgres".to_string());

            pb::Span {
                service: "service1".to_string(),
                name: "query".to_string(),
                resource: "GET /users".to_string(),
                trace_id: 1,
                span_id: 3,
                parent_id: 1,
                start: (aligned_now - BUCKET_SIZE + 20) as i64,
                duration: 75,
                error: 0,
                r#type: "db".to_string(),
                meta,
                metrics: std::collections::HashMap::new(),
                meta_struct: std::collections::HashMap::new(),
                span_links: vec![],
                span_events: vec![],
            }
        },
        // Span with span.kind = server
        {
            let mut meta = std::collections::HashMap::new();
            meta.insert("span.kind".to_string(), "server".to_string());

            let mut metrics = std::collections::HashMap::new();
            metrics.insert("http.status_code".to_string(), 200.0);

            pb::Span {
                service: "service2".to_string(),
                name: "query".to_string(),
                resource: "POST /api/users".to_string(),
                trace_id: 1,
                span_id: 4,
                parent_id: 1,
                start: (aligned_now - BUCKET_SIZE + 30) as i64,
                duration: 200,
                error: 0,
                r#type: "db".to_string(),
                meta,
                metrics,
                meta_struct: std::collections::HashMap::new(),
                span_links: vec![],
                span_events: vec![],
            }
        },
        // Span with measured flag
        {
            let mut metrics = std::collections::HashMap::new();
            metrics.insert("_dd.measured".to_string(), 1.0);

            pb::Span {
                service: "service1".to_string(),
                name: "query".to_string(),
                resource: "database_query".to_string(),
                trace_id: 1,
                span_id: 5,
                parent_id: 1,
                start: (aligned_now - BUCKET_SIZE + 40) as i64,
                duration: 150,
                error: 1,
                r#type: "db".to_string(),
                meta: std::collections::HashMap::new(),
                metrics,
                meta_struct: std::collections::HashMap::new(),
                span_links: vec![],
                span_events: vec![],
            }
        },
        // Grpc span
        {
            let mut meta = std::collections::HashMap::new();
            meta.insert("span.kind".to_string(), "client".to_string());
            meta.insert("rpc.grpc.status_code".to_string(), "aborted".to_string());

            pb::Span {
                service: "service1".to_string(),
                name: "rpc.grpc".to_string(),
                resource: "serviceName.methodName".to_string(),
                trace_id: 1,
                span_id: 3,
                parent_id: 1,
                start: (aligned_now - BUCKET_SIZE + 50) as i64,
                duration: 300,
                error: 0,
                r#type: "rpc".to_string(),
                meta,
                metrics: std::collections::HashMap::new(),
                meta_struct: std::collections::HashMap::new(),
                span_links: vec![],
                span_events: vec![],
            }
        },
    ];

    libdd_trace_utils::trace_utils::compute_top_level_span(pb_spans.as_mut_slice());

    // Add spans to concentrator
    for span in &pb_spans {
        concentrator.add_span(span);
    }

    // Flush and get stats
    let flushtime =
        now + Duration::from_nanos(concentrator.bucket_size * concentrator.buffer_len as u64);
    let stats = concentrator.flush(flushtime, false).all_buckets();

    assert_eq!(stats.len(), 1, "Should get exactly one time bucket");
    let bucket = &stats[0];

    // Validate the stats content
    let expected_stats = vec![
        // Root span stats
        pb::ClientGroupedStats {
            service: "service1".to_string(),
            resource: "GET /users".to_string(),
            r#type: "db".to_string(),
            name: "query".to_string(),
            duration: 100,
            hits: 1,
            top_level_hits: 1,
            errors: 0,
            is_trace_root: pb::Trilean::True.into(),
            ..Default::default()
        },
        // Client span with peer tags
        pb::ClientGroupedStats {
            service: "service1".to_string(),
            resource: "GET /users".to_string(),
            r#type: "db".to_string(),
            name: "query".to_string(),
            span_kind: "client".to_string(),
            peer_tags: vec![
                "db.instance:i-1234".to_string(),
                "db.system:postgres".to_string(),
            ],
            duration: 75,
            hits: 1,
            top_level_hits: 0,
            errors: 0,
            is_trace_root: pb::Trilean::False.into(),
            ..Default::default()
        },
        // Server span
        pb::ClientGroupedStats {
            service: "service2".to_string(),
            resource: "POST /api/users".to_string(),
            r#type: "db".to_string(),
            name: "query".to_string(),
            span_kind: "server".to_string(),
            http_status_code: 200,
            duration: 200,
            hits: 1,
            top_level_hits: 1,
            errors: 0,
            is_trace_root: pb::Trilean::False.into(),
            ..Default::default()
        },
        // Measured span
        pb::ClientGroupedStats {
            service: "service1".to_string(),
            resource: "database_query".to_string(),
            r#type: "db".to_string(),
            name: "query".to_string(),
            duration: 150,
            hits: 1,
            top_level_hits: 0,
            errors: 1,
            is_trace_root: pb::Trilean::False.into(),
            ..Default::default()
        },
        pb::ClientGroupedStats {
            service: "service1".to_string(),
            name: "rpc.grpc".to_string(),
            resource: "serviceName.methodName".to_string(),
            http_status_code: 0,
            r#type: "rpc".to_string(),
            hits: 1,
            errors: 0,
            duration: 300,
            span_kind: "client".to_string(),
            grpc_status_code: "10".to_string(),
            is_trace_root: pb::Trilean::False.into(),
            ..Default::default()
        },
    ];

    assert_counts_equal(expected_stats, bucket.stats.clone());
}

/// Verify the OTLP exact-scalar sidecar tracks per-cell (ok/error) duration/min/max in nanos
/// independently and that ok_duration + error_duration matches the combined group duration
/// (which the agent /v0.6/stats path uses).
#[test]
fn test_flush_with_otlp_exact_per_cell_scalars() {
    let now = SystemTime::now();
    let mut concentrator = SpanConcentrator::new(
        Duration::from_nanos(BUCKET_SIZE),
        now,
        get_span_kinds(),
        vec![],
        None,
        #[cfg(feature = "stats-obfuscation")]
        None,
    );
    // 3 ok spans (200, 300, 100 ns) and 2 error spans (700, 500 ns), all same agg key.
    let mut spans = vec![
        get_test_span(now, 1, 0, 200, 0, "svc", "res", 0),
        get_test_span(now, 2, 0, 300, 0, "svc", "res", 0),
        get_test_span(now, 3, 0, 100, 0, "svc", "res", 0),
        get_test_span(now, 4, 0, 700, 0, "svc", "res", 1),
        get_test_span(now, 5, 0, 500, 0, "svc", "res", 1),
    ];
    compute_top_level_span(spans.as_mut_slice());
    for s in &spans {
        concentrator.add_span(s);
    }

    let flushed = concentrator.flush_with_otlp_exact(now, true);
    assert_eq!(flushed.len(), 1);
    let b = &flushed[0];
    assert_eq!(b.exact.len(), 1);
    let exact = &b.exact[0];

    assert_eq!(exact.ok.count, 3);
    assert_eq!(exact.ok.duration_ns, 600);
    assert_eq!(exact.ok.min_ns, 100);
    assert_eq!(exact.ok.max_ns, 300);

    assert_eq!(exact.error.count, 2);
    assert_eq!(exact.error.duration_ns, 1200);
    assert_eq!(exact.error.min_ns, 500);
    assert_eq!(exact.error.max_ns, 700);

    // ok_duration + error_duration equals the combined group.duration (agent path field).
    let group = &b.bucket.stats[0];
    assert_eq!(
        group.duration,
        exact.ok.duration_ns + exact.error.duration_ns
    );
    assert_eq!(group.hits, 5);
    assert_eq!(group.errors, 2);
}

/// Build a minimal concentrator with a tiny `max_entries_per_bucket` for cardinality tests.
fn make_cardinality_concentrator(cardinality_limits: CardinalityLimitConfig) -> SpanConcentrator {
    let now = SystemTime::now();
    SpanConcentrator::new(
        Duration::from_nanos(BUCKET_SIZE),
        now,
        get_span_kinds(),
        vec!["peer.hostname".to_owned()],
        Some(cardinality_limits),
        #[cfg(feature = "stats-obfuscation")]
        None,
    )
}

/// When the limit is 3 and we insert 5 distinct-resource spans, only 3 normal keys plus one
/// overflow key must appear in the flushed stats. Total hits must equal 5.
#[test]
fn test_whole_key_cardinality_limit_collapse() {
    let now = SystemTime::now();
    let limit: usize = 3;
    let mut concentrator = make_cardinality_concentrator(CardinalityLimitConfig {
        whole_key_limit: limit,
        ..Default::default()
    });

    // Insert limit + 2 distinct-resource root spans all in the same time bucket.
    let resources: Vec<String> = (0..limit + 2).map(|i| format!("resource-{i}")).collect();
    for (i, resource) in resources.iter().enumerate() {
        let span = get_test_span_with_meta(
            now,
            i as u64 + 1,
            0,
            100,
            2,
            "svc",
            resource,
            0,
            &[],
            &[("_dd.measured", 1.0)],
        );
        concentrator.add_span(&span);
    }

    let buckets = concentrator.flush(SystemTime::now(), true).all_buckets();
    assert!(!buckets.is_empty(), "should get at least one time bucket");

    let stats = &buckets[0].stats;

    // Exactly limit normal keys + 1 overflow key.
    assert_eq!(
        stats.len(),
        limit + 1,
        "expected {limit} normal groups + 1 overflow group, got {}",
        stats.len()
    );

    // Total hits must be preserved.
    let total_hits: u64 = stats.iter().map(|g| g.hits).sum();
    assert_eq!(
        total_hits,
        (limit + 2) as u64,
        "total hits must equal the number of inserted spans"
    );

    // Exactly one overflow group, identified by the sentinel resource.
    let overflow_groups: Vec<_> = stats
        .iter()
        .filter(|g| g.resource == TRACER_BLOCKED_VALUE)
        .collect();
    assert_eq!(
        overflow_groups.len(),
        1,
        "expected exactly one overflow group"
    );
}

/// When the `http_endpoint` cardinality limit is 3 and we insert 5 distinct spans differing only on
/// their `http_endpoint`, only 3 normal keys plus one overflow key must appear in the flushed
/// stats.
///
/// But then inserting spans differing on another field, it should not be collapsed.
/// So total hits must equal 6.
#[test]
fn test_per_key_cardinality_limit_collapse_http_endpoint() {
    let now = SystemTime::now();
    let limit: usize = 3;
    let mut concentrator = make_cardinality_concentrator(CardinalityLimitConfig {
        http_endpoint_limit: limit,
        ..Default::default()
    });

    // Insert limit + 2 distinct `http_endpoint` root spans all in the same time bucket.
    let http_endpoints: Vec<String> = (0..limit + 2).map(|i| format!("endpoint-{i}")).collect();
    for (i, http_endpoint) in http_endpoints.iter().enumerate() {
        let meta = [("http.endpoint", http_endpoint.as_str())];
        let span = get_test_span_with_meta(
            now,
            i as u64 + 1,
            0,
            100,
            2,
            "svc",
            "resource",
            0,
            &meta,
            &[("_dd.measured", 1.0)],
        );
        concentrator.add_span(&span);
    }
    // Insert a distinct `resource` root span, this one won't get collapsed
    {
        let meta = [("http.endpoint", "endpoint-0")];
        let span = get_test_span_with_meta(
            now,
            limit as u64 + 3,
            0,
            100,
            2,
            "svc",
            "different-resource",
            0,
            &meta,
            &[("_dd.measured", 1.0)],
        );
        concentrator.add_span(&span);
    }

    let buckets = concentrator.flush(SystemTime::now(), true).all_buckets();
    assert!(!buckets.is_empty(), "should get at least one time bucket");

    let stats = &buckets[0].stats;

    // Exactly limit normal keys + 1 overflow key + the distinct `resource` span
    assert_eq!(
        stats.len(),
        limit + 2,
        "expected {limit} normal groups + 1 overflow key + 1 for the distinct `resource` span, got {}",
        stats.len()
    );

    // Total hits must be preserved.
    let total_hits: u64 = stats.iter().map(|g| g.hits).sum();
    assert_eq!(
        total_hits,
        (limit + 3) as u64,
        "total hits must equal the number of inserted spans"
    );

    // No overflow group, identified by the sentinel resource.
    let overflow_groups: Vec<_> = stats
        .iter()
        .filter(|g| g.resource == TRACER_BLOCKED_VALUE)
        .collect();
    assert_eq!(
        overflow_groups.len(),
        0,
        "expected no overflow group, given whole key cardinality limit was not reached"
    );
    let http_overflow_groups: Vec<_> = stats
        .iter()
        .filter(|g| g.http_endpoint == TRACER_BLOCKED_VALUE)
        .collect();
    assert_eq!(
        http_overflow_groups.len(),
        1,
        "expected exactly one overflow key for the http_endpoint field"
    );
}

/// When whole-key cardinality limit is reached, check that per-key fields are collapsed before
/// falling back to whole-key
#[test]
fn test_per_key_cardinality_limit_collapse_before_whole_key() {
    let now = SystemTime::now();
    let peer_tags_limit = 3;
    let whole_key_limit = peer_tags_limit + 1;
    let mut concentrator = make_cardinality_concentrator(CardinalityLimitConfig {
        whole_key_limit,
        peer_tags_limit,
        ..Default::default()
    });

    // Insert limit + 2 distinct `peer.hostname` root spans all in the same time bucket.
    let inserted_spans = peer_tags_limit + 2;
    let peer_tag_values: Vec<String> = (0..inserted_spans).map(|i| format!("peer-{i}")).collect();
    for (i, peer_tag_value) in peer_tag_values.iter().enumerate() {
        let meta = [
            ("peer.hostname", peer_tag_value.as_str()),
            ("span.kind", "client"),
        ];
        let span = get_test_span_with_meta(
            now,
            i as u64 + 1,
            0,
            100,
            2,
            "svc",
            "resource",
            0,
            &meta,
            &[("_dd.measured", 1.0)],
        );
        concentrator.add_span(&span);
    }

    let buckets = concentrator.flush(SystemTime::now(), true).all_buckets();
    assert!(!buckets.is_empty(), "should get at least one time bucket");

    let stats = &buckets[0].stats;

    // Exactly peer_tags_limit normal keys + 1 overflow key
    assert_eq!(
        stats.len(),
        peer_tags_limit + 1,
        "expected {peer_tags_limit} normal groups + 1 overflow key, got {}",
        stats.len()
    );

    // Total hits must be preserved.
    let total_hits: u64 = stats.iter().map(|g| g.hits).sum();
    assert_eq!(
        total_hits, inserted_spans as u64,
        "total hits must equal the number of inserted spans"
    );

    // No overflow group, identified by the sentinel resource.
    let overflow_groups: Vec<_> = stats
        .iter()
        .filter(|g| g.resource == TRACER_BLOCKED_VALUE)
        .collect();
    assert_eq!(
        overflow_groups.len(),
        0,
        "expected no overflow group: whole key cardinality limit was not reached because per-field cardinality limit collapsed keys before overflowing whole key"
    );
    let peer_tag_overflow_groups: Vec<_> = stats
        .iter()
        .filter(|g| g.peer_tags == [TRACER_BLOCKED_VALUE])
        .collect();
    assert_eq!(
        peer_tag_overflow_groups.len(),
        1,
        "expected exactly one overflow key for the peer_tags field"
    );
}

/// The overflow bucket must correctly aggregate the hits from overflow spans.
#[test]
fn test_overflow_bucket_counts() {
    let now = SystemTime::now();
    let limit: usize = 1;
    let mut concentrator = make_cardinality_concentrator(CardinalityLimitConfig {
        whole_key_limit: limit,
        ..Default::default()
    });

    // First span fills the sole slot; the next 4 spans all have distinct keys → all overflow.
    for i in 0..5usize {
        let resource = format!("resource-{i}");
        let span = get_test_span_with_meta(
            now,
            i as u64 + 1,
            0,
            10 * (i as i64 + 1),
            2,
            "svc",
            &resource,
            0,
            &[],
            &[("_dd.measured", 1.0)],
        );
        concentrator.add_span(&span);
    }

    let buckets = concentrator.flush(SystemTime::now(), true).all_buckets();
    assert!(!buckets.is_empty());
    let stats = &buckets[0].stats;

    // There must be exactly 2 groups: 1 normal + 1 overflow.
    assert_eq!(
        stats.len(),
        2,
        "expected exactly 1 normal + 1 overflow group"
    );

    let overflow = stats
        .iter()
        .find(|g| g.resource == TRACER_BLOCKED_VALUE)
        .expect("overflow group must exist");

    // 4 spans overflowed, total duration = 20 + 30 + 40 + 50 = 140.
    assert_eq!(overflow.hits, 4, "all 4 overflow spans must be merged");
    assert_eq!(
        overflow.duration, 140,
        "overflow durations must sum correctly"
    );
}

/// When the number of distinct spans is within the limit, no overflow bucket should appear.
#[test]
fn test_no_collapse_within_limit() {
    let now = SystemTime::now();
    let limit: usize = 10;
    let mut concentrator = make_cardinality_concentrator(CardinalityLimitConfig {
        whole_key_limit: limit,
        ..Default::default()
    });

    // Insert exactly `limit` distinct-resource spans — no overflow expected.
    for i in 0..limit {
        let resource = format!("resource-{i}");
        let span = get_test_span_with_meta(
            now,
            i as u64 + 1,
            0,
            50,
            2,
            "svc",
            &resource,
            0,
            &[],
            &[("_dd.measured", 1.0)],
        );
        concentrator.add_span(&span);
    }

    let buckets = concentrator.flush(SystemTime::now(), true).all_buckets();
    assert!(!buckets.is_empty());
    let stats = &buckets[0].stats;

    assert_eq!(
        stats.len(),
        limit,
        "expected exactly {limit} groups with no overflow"
    );
    assert!(
        stats.iter().all(|g| g.resource != TRACER_BLOCKED_VALUE),
        "no overflow group should be present within the limit"
    );
}

/// The overflow `ClientGroupedStats` row must carry `tracer_blocked_value` on all sentinel
/// string fields as specified by the RFC.
#[test]
fn test_overflow_bucket_key_sentinel_values() {
    let now = SystemTime::now();
    let limit: usize = 1;
    let mut concentrator = make_cardinality_concentrator(CardinalityLimitConfig {
        whole_key_limit: limit,
        ..Default::default()
    });

    // First span occupies the only slot; second one overflows.
    let first = get_test_span_with_meta(
        now,
        1,
        0,
        50,
        2,
        "my-service",
        "my-resource",
        0,
        &[],
        &[("_dd.measured", 1.0)],
    );
    let second = get_test_span_with_meta(
        now,
        2,
        0,
        75,
        2,
        "other-service",
        "other-resource",
        0,
        &[],
        &[("_dd.measured", 1.0)],
    );

    concentrator.add_span(&first);
    concentrator.add_span(&second);

    let buckets = concentrator.flush(SystemTime::now(), true).all_buckets();
    assert!(!buckets.is_empty());
    let stats = &buckets[0].stats;

    let overflow = stats
        .iter()
        .find(|g| g.resource == TRACER_BLOCKED_VALUE)
        .expect("overflow group must exist");

    // Every string dimension must be the sentinel.
    assert_eq!(
        overflow.service, TRACER_BLOCKED_VALUE,
        "service must be sentinel"
    );
    assert_eq!(overflow.name, TRACER_BLOCKED_VALUE, "name must be sentinel");
    assert_eq!(
        overflow.resource, TRACER_BLOCKED_VALUE,
        "resource must be sentinel"
    );
    assert_eq!(
        overflow.r#type, TRACER_BLOCKED_VALUE,
        "type must be sentinel"
    );
    assert_eq!(
        overflow.span_kind, TRACER_BLOCKED_VALUE,
        "span_kind must be sentinel"
    );
    assert_eq!(
        overflow.http_method, TRACER_BLOCKED_VALUE,
        "http_method must be sentinel"
    );
    assert_eq!(
        overflow.http_endpoint, TRACER_BLOCKED_VALUE,
        "http_endpoint must be sentinel"
    );
    assert_eq!(
        overflow.service_source, TRACER_BLOCKED_VALUE,
        "service_source must be sentinel"
    );
    // Numeric and boolean fields must be zero/false (NOT_SET per RFC).
    assert_eq!(overflow.http_status_code, 0, "http_status_code must be 0");
    assert_eq!(
        overflow.grpc_status_code, "",
        "grpc_status_code must be empty"
    );
    assert!(!overflow.synthetics, "synthetics must be false");
    // is_trace_root uses Trilean; NOT_SET maps to 0.
    assert_eq!(
        overflow.is_trace_root, 0,
        "is_trace_root must be NOT_SET (0)"
    );
    assert_eq!(overflow.peer_tags, [TRACER_BLOCKED_VALUE]);

    // The normal group must be unaffected.
    let normal = stats
        .iter()
        .find(|g| g.resource != TRACER_BLOCKED_VALUE)
        .expect("normal group must exist");
    assert_eq!(normal.service, "my-service");
    assert_eq!(normal.resource, "my-resource");
}
