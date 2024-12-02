// Copyright 2024-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0
//! This module implements the SpanConcentrator used to aggregate spans into stats
use std::collections::HashMap;
use std::time::{self, Duration, SystemTime};

use datadog_trace_protobuf::pb;
use datadog_trace_utils::span_v04::{trace_utils, Span};

use aggregation::{AggregationKey, StatsBucket};

mod aggregation;

/// Return a Duration between t and the unix epoch
/// If t is before the unix epoch return 0
fn system_time_to_unix_duration(t: SystemTime) -> Duration {
    t.duration_since(time::UNIX_EPOCH)
        .unwrap_or(Duration::from_nanos(0))
}

/// Align a timestamp on the start of a bucket
#[inline]
fn align_timestamp(t: u64, bucket_size: u64) -> u64 {
    t - (t % bucket_size)
}

/// Return true if the span has a span.kind that is eligible for stats computation
fn compute_stats_for_span_kind(span: &Span, span_kinds_stats_computed: &[String]) -> bool {
    !span_kinds_stats_computed.is_empty()
        && span.meta.get("span.kind").is_some_and(|span_kind| {
            span_kinds_stats_computed.contains(&span_kind.as_str().to_lowercase())
        })
}

/// Return true if the span should be ignored for stats computation
fn should_ignore_span(span: &Span, span_kinds_stats_computed: &[String]) -> bool {
    !(trace_utils::has_top_level(span)
        || trace_utils::is_measured(span)
        || compute_stats_for_span_kind(span, span_kinds_stats_computed))
        || trace_utils::is_partial_snapshot(span)
}

/// SpanConcentrator compute stats on span aggregated by time and span attributes
///
/// # Aggregation
/// Spans are aggregated into time buckets based on their end_time. Within each time bucket there
/// is another level of aggregation based on the spans fields (e.g. resource_name, service_name)
/// and the peer tags if the `peer_tags_aggregation` is enabled.
///
/// # Span eligibility
/// The ingested spans are only aggregated if they are root, top-level, measured or if their
/// `span.kind` is eligible and the `compute_stats_by_span_kind` is enabled.
///
/// # Flushing
/// When the SpanConcentrator is flushed it keeps the `buffer_len` most recent buckets and remove
/// all older buckets returning their content. When using force flush all buckets are flushed
/// regardless of their age.
#[derive(Debug, Clone)]
pub struct SpanConcentrator {
    /// Size of the time buckets used for aggregation in nanos
    bucket_size: u64,
    buckets: HashMap<u64, StatsBucket>,
    /// Timestamp of the oldest time bucket for which we allow data.
    /// Any ingested stats older than it get added to this bucket.
    oldest_timestamp: u64,
    /// bufferLen is the number stats bucket we keep when flushing.
    buffer_len: usize,
    /// span.kind fields eligible for stats computation
    span_kinds_stats_computed: Vec<String>,
    /// keys for supplementary tags that describe peer.service entities
    peer_tag_keys: Vec<String>,
}

impl SpanConcentrator {
    /// Return a new concentrator with the given parameters
    /// - `bucket_size` is the size of the time buckets
    /// - `now` the current system time, used to define the oldest bucket
    /// - `span_kinds_stats_computed` list of span kinds eligible for stats computation
    /// - `peer_tags_keys` list of keys considered as peer tags for aggregation
    pub fn new(
        bucket_size: Duration,
        now: SystemTime,
        span_kinds_stats_computed: Vec<String>,
        peer_tag_keys: Vec<String>,
    ) -> SpanConcentrator {
        SpanConcentrator {
            bucket_size: bucket_size.as_nanos() as u64,
            buckets: HashMap::new(),
            oldest_timestamp: align_timestamp(
                system_time_to_unix_duration(now).as_nanos() as u64,
                bucket_size.as_nanos() as u64,
            ),
            buffer_len: 2,
            span_kinds_stats_computed,
            peer_tag_keys,
        }
    }

    /// Set the list of span kinds eligible for stats computation
    pub fn set_span_kinds(&mut self, span_kinds: Vec<String>) {
        self.span_kinds_stats_computed = span_kinds;
    }

    /// Set the list of keys considered as peer_tags for aggregation
    pub fn set_peer_tags(&mut self, peer_tags: Vec<String>) {
        self.peer_tag_keys = peer_tags;
    }

    /// Return the bucket size used for aggregation
    pub fn get_bucket_size(&self) -> Duration {
        Duration::from_nanos(self.bucket_size)
    }

    /// Add a span into the concentrator, by computing stats if the span is elligible for stats
    /// computation.
    pub fn add_span(&mut self, span: &Span) {
        // If the span is elligible for stats computation
        if !should_ignore_span(span, self.span_kinds_stats_computed.as_slice()) {
            let mut bucket_timestamp =
                align_timestamp((span.start + span.duration) as u64, self.bucket_size);
            // If the span is to old we aggregate it in the latest bucket instead of
            // creating a new one
            if bucket_timestamp < self.oldest_timestamp {
                bucket_timestamp = self.oldest_timestamp;
            }

            let temp_field_should_not_be_commited: Vec<&str> =
                self.peer_tag_keys.iter().map(|s| s as &str).collect();

            let agg_key =
                AggregationKey::from_span(span, temp_field_should_not_be_commited.as_slice());

            self.buckets
                .entry(bucket_timestamp)
                .or_insert(StatsBucket::new(bucket_timestamp))
                .insert(agg_key, span);
        }
    }

    /// Flush all stats bucket except for the `buffer_len` most recent. If `force` is true, flush
    /// all buckets.
    pub fn flush(&mut self, now: SystemTime, force: bool) -> Vec<pb::ClientStatsBucket> {
        // TODO: Use drain filter from hashbrown to avoid removing current buckets
        let now_timestamp = system_time_to_unix_duration(now).as_nanos() as u64;
        let buckets: Vec<(u64, StatsBucket)> = self.buckets.drain().collect();
        self.oldest_timestamp = if force {
            align_timestamp(now_timestamp, self.bucket_size)
        } else {
            align_timestamp(now_timestamp, self.bucket_size)
                - (self.buffer_len as u64 - 1) * self.bucket_size
        };
        buckets
            .into_iter()
            .filter_map(|(timestamp, bucket)| {
                // Always keep `bufferLen` buckets (default is 2: current + previous one).
                // This is a trade-off: we accept slightly late traces (clock skew and stuff)
                // but we delay flushing by at most `bufferLen` buckets.
                // This delay might result in not flushing stats payload (data loss)
                // if the tracer stops while the latest buckets aren't old enough to be flushed.
                // The "force" boolean skips the delay and flushes all buckets, typically on
                // shutdown.
                if !force && timestamp > (now_timestamp - self.buffer_len as u64 * self.bucket_size)
                {
                    self.buckets.insert(timestamp, bucket);
                    return None;
                }
                Some(bucket.flush(self.bucket_size))
            })
            .collect()
    }
}

#[cfg(test)]
mod tests;
