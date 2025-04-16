// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use super::*;
use datadog_trace_utils::span::{trace_utils::compute_top_level_span, SpanSlice};
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
        service: service,
        name: "query".into(),
        resource: resource,
        error,
        r#type: "db".into(),
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
    span.metrics = HashMap::new();
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
        expected_map.insert(AggregationKey::from(group.clone()), group);
    });
    actual.into_iter().for_each(|mut group| {
        group.ok_summary = vec![];
        group.error_summary = vec![];
        actual_map.insert(AggregationKey::from(group.clone()), group);
    });
    assert_eq!(expected_map, actual_map)
}

/// Test that the SpanConcentrator does not create buckets older than the exporter initialization
#[test]
fn test_concentrator_oldest_timestamp_cold() {
    let now = SystemTime::now();
    let mut concentrator =
        SpanConcentrator::new(Duration::from_nanos(BUCKET_SIZE), now, vec![], vec![]);
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
        let stats = concentrator.flush(flushtime, false);
        assert_eq!(stats.len(), 0, "We should get 0 time buckets");
        flushtime += Duration::from_nanos(concentrator.bucket_size);
    }

    let stats = concentrator.flush(flushtime, false);

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
    let mut concentrator =
        SpanConcentrator::new(Duration::from_nanos(BUCKET_SIZE), now, vec![], vec![]);
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
        let stats = concentrator.flush(flushtime, false);
        assert!(stats.is_empty(), "We should get 0 time buckets");
        flushtime += Duration::from_nanos(concentrator.bucket_size);
    }

    let stats = concentrator.flush(flushtime, false);
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
    let stats = concentrator.flush(flushtime, false);
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
    let mut concentrator =
        SpanConcentrator::new(Duration::from_nanos(BUCKET_SIZE), now, vec![], vec![]);
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
        let stats = concentrator.flush(flushtime, false);
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
    let mut concentrator =
        SpanConcentrator::new(Duration::from_nanos(BUCKET_SIZE), now, vec![], vec![]);
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
        let stats = concentrator.flush(flushtime, false);
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

        let stats = concentrator.flush(flushtime, false);
        assert_eq!(
            stats.len(),
            0,
            "Second flush on the same time should be empty"
        );
        flushtime += Duration::from_nanos(concentrator.bucket_size);
    }
}

/// Test the criterias to include a span in stats computation
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

    let stats = concentrator.flush(
        now + Duration::from_nanos(concentrator.bucket_size * concentrator.buffer_len as u64),
        false,
    );
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
        .insert("_dd.partial_version".into(), 830604.0);
    compute_top_level_span(spans.as_mut_slice());
    let mut concentrator = SpanConcentrator::new(
        Duration::from_nanos(BUCKET_SIZE),
        now,
        get_span_kinds(),
        vec![],
    );
    for span in &spans {
        concentrator.add_span(span);
    }

    let stats = concentrator.flush(
        now + Duration::from_nanos(concentrator.bucket_size * concentrator.buffer_len as u64),
        false,
    );
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
    );
    for span in &spans {
        concentrator.add_span(span);
    }

    // flushtime is 1h before now to make sure the bucket is not old enough to be flushed
    // without force flush
    let flushtime = now - Duration::from_secs(3600);

    // Bucket should not be flushed without force flush
    let stats = concentrator.flush(flushtime, false);
    assert_eq!(0, stats.len());

    let stats = concentrator.flush(flushtime, true);
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
    );
    let mut concentrator_with_peer_tags = SpanConcentrator::new(
        Duration::from_nanos(BUCKET_SIZE),
        now,
        get_span_kinds(),
        vec!["db.instance".to_string(), "db.system".to_string()],
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

    let stats_with_peer_tags = concentrator_with_peer_tags.flush(flushtime, false);
    assert_counts_equal(
        expected_with_peer_tags,
        stats_with_peer_tags
            .first()
            .expect("There should be at least one time bucket")
            .stats
            .clone(),
    );

    let stats_without_peer_tags = concentrator_without_peer_tags.flush(flushtime, false);
    assert_counts_equal(
        expected_without_peer_tags,
        stats_without_peer_tags
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
                meta: HashMap::from([("span.kind".into(), "server".into())]),
                ..Default::default()
            },
            true,
        ),
        (
            SpanSlice {
                meta: HashMap::from([("span.kind".into(), "consumer".into())]),
                ..Default::default()
            },
            true,
        ),
        (
            SpanSlice {
                meta: HashMap::from([("span.kind".into(), "client".into())]),
                ..Default::default()
            },
            true,
        ),
        (
            SpanSlice {
                meta: HashMap::from([("span.kind".into(), "producer".into())]),
                ..Default::default()
            },
            true,
        ),
        (
            SpanSlice {
                meta: HashMap::from([("span.kind".into(), "internal".into())]),
                ..Default::default()
            },
            false,
        ),
        (
            SpanSlice {
                meta: HashMap::from([("span.kind".into(), "SERVER".into())]),
                ..Default::default()
            },
            true,
        ),
        (
            SpanSlice {
                meta: HashMap::from([("span.kind".into(), "CONSUMER".into())]),
                ..Default::default()
            },
            true,
        ),
        (
            SpanSlice {
                meta: HashMap::from([("span.kind".into(), "CLIENT".into())]),
                ..Default::default()
            },
            true,
        ),
        (
            SpanSlice {
                meta: HashMap::from([("span.kind".into(), "PRODUCER".into())]),
                ..Default::default()
            },
            true,
        ),
        (
            SpanSlice {
                meta: HashMap::from([("span.kind".into(), "INTERNAL".into())]),
                ..Default::default()
            },
            false,
        ),
        (
            SpanSlice {
                meta: HashMap::from([("span.kind".into(), "SerVER".into())]),
                ..Default::default()
            },
            true,
        ),
        (
            SpanSlice {
                meta: HashMap::from([("span.kind".into(), "ConSUMeR".into())]),
                ..Default::default()
            },
            true,
        ),
        (
            SpanSlice {
                meta: HashMap::from([("span.kind".into(), "CLiENT".into())]),
                ..Default::default()
            },
            true,
        ),
        (
            SpanSlice {
                meta: HashMap::from([("span.kind".into(), "PROducER".into())]),
                ..Default::default()
            },
            true,
        ),
        (
            SpanSlice {
                meta: HashMap::from([("span.kind".into(), "INtERNAL".into())]),
                ..Default::default()
            },
            false,
        ),
        (
            SpanSlice {
                meta: HashMap::from([("span.kind".into(), "".into())]),
                ..Default::default()
            },
            false,
        ),
        (
            SpanSlice {
                meta: HashMap::from([]),
                ..Default::default()
            },
            false,
        ),
    ];

    for (span, is_eligible) in test_cases {
        assert!(compute_stats_for_span_kind(&span, &get_span_kinds()) == is_eligible)
    }
}
