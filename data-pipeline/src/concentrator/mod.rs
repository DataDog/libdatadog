// Copyright 2024-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0
//! This module implements the Concentrator used to aggregate spans into stats
#![allow(dead_code)] // TODO: Remove once the trace exporter uses the concentrator
use std::collections::HashMap;
use std::time::{self, Duration, SystemTime};

use anyhow::{anyhow, Result};
use datadog_trace_protobuf::pb;
use datadog_trace_utils::trace_utils;

use aggregation::{AggregationKey, StatsBucket};

mod aggregation;

/// Return a Duration between t and the unix epoch
/// If t is before the unix epoch return 0
fn system_time_to_unix_duration(t: SystemTime) -> Duration {
    match t.duration_since(time::UNIX_EPOCH) {
        Err(_) => Duration::from_nanos(0),
        Ok(d) => d,
    }
}

/// Align a timestamp on the start of a bucket
#[inline]
fn align_timestamp(t: u64, bucket_size: u64) -> u64 {
    t - (t % bucket_size)
}

/// Return true if the span has a span.kind that is eligible for stats computation
fn compute_stats_for_span_kind(span: &pb::Span) -> bool {
    span.meta.get("span.kind").is_some_and(|span_kind| {
        matches!(
            span_kind.to_lowercase().as_str(),
            "server" | "consumer" | "client" | "producer"
        )
    })
}

fn should_ignore_span(span: &pb::Span, compute_stats_by_span_kind: bool) -> bool {
    !(trace_utils::has_top_level(span)
        || trace_utils::is_measured(span)
        || (compute_stats_by_span_kind && compute_stats_for_span_kind(span)))
        || trace_utils::is_partial_snapshot(span)
}

/// The concentrator compute stats on span aggregated by time and span attributes
///
/// The ingested spans are only aggregated if they are root, top-level, measured or if their
/// `span.kind` is eligible and the `compute_stats_by_span_kind` is enabled.
#[derive(Debug)]
pub struct Concentrator {
    /// Size of the time buckets used for aggregation in nanos
    bucket_size: u64,
    buckets: HashMap<u64, StatsBucket>,
    /// Timestamp of the oldest time bucket for which we allow data.
    /// Any ingested stats older than it get added to this bucket.
    oldest_timestamp: u64,
    /// bufferLen is the number of 10s stats bucket we keep in memory before flushing them.
    /// It means that we can compute stats only for the last `bufferLen * bsize` and that we
    /// wait such time before flushing the stats.
    /// This only applies to past buckets. Stats buckets in the future are allowed with no
    /// restriction.
    buffer_len: usize,
    /// flag to enable aggregation of peer tags
    peer_tags_aggregation: bool,
    /// flag to enable computation of stats through checking the span.kind field
    compute_stats_by_span_kind: bool,
    /// keys for supplementary tags that describe peer.service entities
    peer_tag_keys: Vec<String>,
}

impl Concentrator {
    /// Return a new concentrator with the given parameter
    /// - `bucket_size`
    pub fn new(
        bucket_size: Duration,
        now: SystemTime,
        peer_tags_aggregation: bool,
        compute_stats_by_span_kind: bool,
        peer_tag_keys: Vec<String>,
    ) -> Concentrator {
        Concentrator {
            bucket_size: bucket_size.as_nanos() as u64,
            buckets: HashMap::new(),
            oldest_timestamp: align_timestamp(
                system_time_to_unix_duration(now).as_nanos() as u64,
                bucket_size.as_nanos() as u64,
            ),
            buffer_len: 2,
            peer_tags_aggregation,
            compute_stats_by_span_kind,
            peer_tag_keys,
        }
    }

    pub fn add_span(&mut self, span: &pb::Span) -> Result<()> {
        if should_ignore_span(span, self.compute_stats_by_span_kind) {
            return Ok(()); // Span is ignored
        }
        if let Ok(end_time) = u64::try_from(span.start + span.duration) {
            let mut bucket_timestamp = align_timestamp(end_time, self.bucket_size);
            // If the span is to old we aggregate it in the latest bucket instead of
            // creating a new one
            if bucket_timestamp < self.oldest_timestamp {
                bucket_timestamp = self.oldest_timestamp;
            }

            let agg_key =
                AggregationKey::from_span(span, self.peer_tags_aggregation, &self.peer_tag_keys);

            self.buckets
                .entry(bucket_timestamp)
                .or_insert(StatsBucket::new(bucket_timestamp))
                .insert(agg_key, span);

            Ok(())
        } else {
            Err(anyhow!("Invalid span endtime"))
        }
    }

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
                // The "force" boolean skips the delay and flushes all buckets, typically on agent
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
