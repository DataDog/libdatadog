// Copyright 2024-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0
//! This module implements the SpanConcentrator used to aggregate spans into stats
use std::collections::HashMap;
use std::time::{self, Duration, SystemTime};
use tracing::{debug, warn};

use libdd_trace_protobuf::pb;

use aggregation::StatsBucket;

mod aggregation;
use aggregation::BorrowedAggregationKey;
pub use aggregation::{
    FixedAggregationKey, OtlpExactCell, OtlpExactGroup, OtlpStatsBucket,
    StatsBucketCollapseTelemetry,
};

pub mod stat_span;
pub use stat_span::StatSpan;

/// Result of flushing a concentrator.
///
/// Obfuscated and un-obfuscated buckets are kept separate because they must be sent in distinct
/// stats payloads: only the obfuscated payload carries the `datadog-obfuscation-version` header.
pub struct FlushResult {
    /// Buckets whose resource names were obfuscated client-side.
    pub obfuscated_buckets: Vec<pb::ClientStatsBucket>,
    /// Buckets whose resource names were left as-is.
    pub unobfuscated_buckets: Vec<pb::ClientStatsBucket>,
    /// Total number of spans that were collapsed into the overflow sentinel bucket due to
    /// whole-key cardinality limiting across all flushed time buckets.
    pub collapsed_spans: StatsBucketCollapseTelemetry,
}

impl FlushResult {
    /// All flushed buckets regardless of obfuscation.
    #[cfg(test)]
    pub fn all_buckets(self) -> Vec<pb::ClientStatsBucket> {
        let mut buckets = self.obfuscated_buckets;
        buckets.extend(self.unobfuscated_buckets);
        buckets
    }
}

/// Concentrators that can provide raw time buckets for export implement this trait.
///
/// `StatsExporter` is generic over `C: FlushableConcentrator` so it can work with
/// both the in-process [`SpanConcentrator`] and the SHM-backed `ShmSpanConcentrator`.
pub trait FlushableConcentrator {
    /// Flush time buckets and return them together with flush metadata. If `force` is true, flush
    /// all buckets. See [`FlushResult`] for the returned data.
    fn flush_buckets(&mut self, force: bool) -> FlushResult;
}

impl FlushableConcentrator for SpanConcentrator {
    fn flush_buckets(&mut self, force: bool) -> FlushResult {
        self.flush(SystemTime::now(), force)
    }
}

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

/// Return true if the span is eligible for stats computation
pub fn is_span_eligible<'a, T>(span: &'a T, span_kinds_stats_computed: &[String]) -> bool
where
    T: StatSpan<'a>,
{
    (span.has_top_level() || span.is_measured() || {
        span.get_meta("span.kind")
            .is_some_and(|span_kind| span_kinds_stats_computed.contains(&span_kind.to_lowercase()))
    }) && !span.is_partial_snapshot()
}

#[cfg(feature = "stats-obfuscation")]
#[derive(Clone, Debug, Default)]
#[cfg_attr(target_arch = "wasm32", allow(dead_code))]
pub struct StatsComputationObfuscationConfig {
    pub enabled: bool,
    pub sql_obfuscation_mode: libdd_trace_obfuscation::sql::SqlObfuscationMode,
}

#[cfg(feature = "stats-obfuscation")]
pub type SharedStatsComputationObfuscationConfig =
    std::sync::Arc<arc_swap::ArcSwap<StatsComputationObfuscationConfig>>;

/// Default maximum number of distinct aggregation keys per time bucket.
///
/// 7 168 is the limit to exactly saturate hashbrown's internal table at its maximum load factor of
/// 7/8. Any higher limit would immediately force a doubling of the table capacity, wasting
/// half the allocated slots for a modest increase in cardinality. To avoid future changes going
/// over this limit (e.g. adding extra overflow buckets) we set a slightly lower limit.
pub const DEFAULT_MAX_ENTRIES_PER_BUCKET: usize = 7_000;

/// Config to override the default stats cardinality limit values
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(C)]
pub struct CardinalityLimitConfig {
    /// The whole-key cardinality limit (defaults to 7000)
    pub whole_key_limit: usize,
    /// The per-field cardinality limit for the Resource field (defaults to 1024)
    pub resource_limit: usize,
    /// The per-field cardinality limit for the HttpEndpoint field (defaults to 512)
    pub http_endpoint_limit: usize,
    /// The per-field cardinality limit for the PeerTags field (defaults to 512)
    pub peer_tags_limit: usize,
    /// The per-field cardinality limit for the AdditionalTags field (defaults to 100)
    pub additional_tags_limit: usize,
}

impl Default for CardinalityLimitConfig {
    fn default() -> Self {
        Self {
            whole_key_limit: DEFAULT_MAX_ENTRIES_PER_BUCKET,
            resource_limit: 1024,
            http_endpoint_limit: 512,
            peer_tags_limit: 512,
            additional_tags_limit: 100,
        }
    }
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
///
/// # Cardinality limiting
/// Each time bucket holds at most `max_entries_per_bucket` distinct aggregation keys. Once that
/// limit is reached, spans whose key is not already present are merged into a single overflow
/// bucket keyed by [`aggregation::TRACER_BLOCKED_VALUE`].
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
    /// Config values for whole-key and per-field cardinality limits
    cardinality_limits: CardinalityLimitConfig,
    /// span.kind fields eligible for stats computation
    span_kinds_stats_computed: Vec<String>,
    /// keys for supplementary tags that describe peer.service entities
    peer_tag_keys: Vec<String>,
    /// Cardinality collapsed log was already emitted
    cardinality_log_emitted: bool,
    #[cfg(feature = "stats-obfuscation")]
    obfuscation_config: SharedStatsComputationObfuscationConfig,
}

impl SpanConcentrator {
    /// Return a new concentrator with the given parameters
    /// - `bucket_size` is the size of the time buckets
    /// - `now` the current system time, used to define the oldest bucket
    /// - `span_kinds_stats_computed` list of span kinds eligible for stats computation
    /// - `peer_tags_keys` list of keys considered as peer tags for aggregation
    /// - `override_max_entries_per_bucket` maximum distinct aggregation keys per time bucket before
    ///   cardinality limiting applies. Pass `None` to use [`DEFAULT_MAX_ENTRIES_PER_BUCKET`].
    /// - `obfuscation_config` optional and updatable config for resource key obfuscation
    pub fn new(
        bucket_size: Duration,
        now: SystemTime,
        span_kinds_stats_computed: Vec<String>,
        peer_tag_keys: Vec<String>,
        override_cardinality_limits: Option<CardinalityLimitConfig>,
        #[cfg(feature = "stats-obfuscation")] obfuscation_config: Option<
            SharedStatsComputationObfuscationConfig,
        >,
    ) -> SpanConcentrator {
        if let Some(cardinality_limit_config) = override_cardinality_limits.as_ref() {
            if cardinality_limit_config.whole_key_limit <= cardinality_limit_config.resource_limit
                || cardinality_limit_config.whole_key_limit
                    <= cardinality_limit_config.http_endpoint_limit
                || cardinality_limit_config.whole_key_limit
                    <= cardinality_limit_config.peer_tags_limit
                || cardinality_limit_config.whole_key_limit
                    <= cardinality_limit_config.additional_tags_limit
            {
                warn!(
                    "Stats cardinality limit is misconfigured: per-field limits must be lower than whole-key limit otherwise they have no effect and you will get over-collapsed stats!"
                );
            }
        }
        SpanConcentrator {
            bucket_size: bucket_size.as_nanos() as u64,
            buckets: HashMap::new(),
            oldest_timestamp: align_timestamp(
                system_time_to_unix_duration(now).as_nanos() as u64,
                bucket_size.as_nanos() as u64,
            ),
            buffer_len: 2,
            cardinality_limits: override_cardinality_limits.unwrap_or_default(),
            span_kinds_stats_computed,
            peer_tag_keys,
            #[cfg(feature = "stats-obfuscation")]
            obfuscation_config: obfuscation_config.unwrap_or_default(),
            cardinality_log_emitted: false,
        }
    }

    /// Return the list of span kinds eligible for stats computation
    pub fn span_kinds(&self) -> &[String] {
        &self.span_kinds_stats_computed
    }

    /// Set the list of span kinds eligible for stats computation
    pub fn set_span_kinds(&mut self, span_kinds: Vec<String>) {
        self.span_kinds_stats_computed = span_kinds;
    }

    /// Return the list of keys considered as peer_tags for aggregation
    pub fn peer_tag_keys(&self) -> &[String] {
        &self.peer_tag_keys
    }

    /// Set the list of keys considered as peer_tags for aggregation
    pub fn set_peer_tags(&mut self, peer_tags: Vec<String>) {
        self.peer_tag_keys = peer_tags;
    }

    /// Return the bucket size used for aggregation
    pub fn get_bucket_size(&self) -> Duration {
        Duration::from_nanos(self.bucket_size)
    }

    /// Add a span into the concentrator, by computing stats if the span is eligible for stats
    /// computation.
    pub fn add_span<'a>(&'a mut self, span: &'a impl StatSpan<'a>) {
        if !is_span_eligible(span, self.span_kinds_stats_computed.as_slice()) {
            return;
        }
        let mut bucket_timestamp =
            align_timestamp((span.start() + span.duration()) as u64, self.bucket_size);
        // If the span is to old we aggregate it in the latest bucket instead of
        // creating a new one
        if bucket_timestamp < self.oldest_timestamp {
            bucket_timestamp = self.oldest_timestamp;
        }

        let target_bucket = self.buckets.entry(bucket_timestamp).or_insert_with(|| {
            StatsBucket::new(
                bucket_timestamp,
                self.cardinality_limits,
                #[cfg(feature = "stats-obfuscation")]
                self.obfuscation_config.load().enabled,
            )
        });
        #[cfg(feature = "stats-obfuscation")]
        let obfuscated_resource = if target_bucket.obfuscated {
            Self::compute_obfuscated_span(self.obfuscation_config.load().sql_obfuscation_mode, span)
        } else {
            None
        };
        #[cfg(not(feature = "stats-obfuscation"))]
        let obfuscated_resource: Option<String> = None;
        let agg_key = match obfuscated_resource.as_deref() {
            Some(res) => BorrowedAggregationKey::from_obfuscated_span(
                res,
                span,
                self.peer_tag_keys.as_slice(),
            ),
            None => BorrowedAggregationKey::from_span(span, self.peer_tag_keys.as_slice()),
        };
        target_bucket.insert(
            agg_key,
            span.duration(),
            span.is_error(),
            span.has_top_level(),
        );
    }

    #[cfg(feature = "stats-obfuscation")]
    fn compute_obfuscated_span<'a>(
        sql_obfuscation_mode: libdd_trace_obfuscation::sql::SqlObfuscationMode,
        span: &'a impl StatSpan<'a>,
    ) -> Option<String> {
        let dbms_hint: Option<&str> = span.get_meta("db.type");
        libdd_trace_obfuscation::obfuscate::obfuscate_resource_for_stats(
            span.r#type(),
            span.resource(),
            dbms_hint,
            sql_obfuscation_mode,
        )
    }

    /// Flush all stats bucket except for the `buffer_len` most recent. If `force` is true, flush
    /// all buckets.
    ///
    /// Obfuscated and un-obfuscated buckets are returned separately, see [`FlushResult`].
    pub fn flush(&mut self, now: SystemTime, force: bool) -> FlushResult {
        let (buckets, collapsed_spans) = self.drain_due_buckets(now, force, StatsBucket::flush);
        let mut obfuscated_buckets = Vec::new();
        let mut unobfuscated_buckets = Vec::new();
        for (obfuscated, bucket) in buckets {
            if obfuscated {
                obfuscated_buckets.push(bucket);
            } else {
                unobfuscated_buckets.push(bucket);
            }
        }
        FlushResult {
            obfuscated_buckets,
            unobfuscated_buckets,
            collapsed_spans,
        }
    }

    /// Like [`Self::flush`], but also emits exact per-cell scalars alongside each bucket for the
    /// OTLP trace-metrics path. The protobuf bucket inside each [`OtlpStatsBucket`] is identical
    /// to what [`Self::flush`] would produce, so the /v0.6/stats agent path is unaffected.
    pub fn flush_with_otlp_exact(&mut self, now: SystemTime, force: bool) -> Vec<OtlpStatsBucket> {
        let (buckets, _) = self.drain_due_buckets(now, force, StatsBucket::flush_with_otlp_exact);
        buckets.into_iter().map(|(_, bucket)| bucket).collect()
    }

    /// Drain the buckets that are due for flushing, encoding each with `encode`.
    ///
    /// Returns a tuple `(buckets, collapsed_spans)` where each encoded bucket is paired with a
    /// boolean indicating whether it was obfuscated client-side (always `false` when the
    /// `stats-obfuscation` feature is disabled), and `collapsed_spans` is the total number of
    /// spans collapsed into the overflow sentinel bucket due to cardinality limiting.
    fn drain_due_buckets<T>(
        &mut self,
        now: SystemTime,
        force: bool,
        encode: impl Fn(StatsBucket, u64) -> T,
    ) -> (Vec<(bool, T)>, StatsBucketCollapseTelemetry) {
        // TODO: Wait for HashMap::extract_if to be stabilized to avoid a full drain
        let now_timestamp = system_time_to_unix_duration(now).as_nanos() as u64;
        let buckets: Vec<(u64, StatsBucket)> = self.buckets.drain().collect();
        self.oldest_timestamp = if force {
            align_timestamp(now_timestamp, self.bucket_size)
        } else {
            align_timestamp(now_timestamp, self.bucket_size)
                - (self.buffer_len as u64 - 1) * self.bucket_size
        };
        let mut total_collapsed = StatsBucketCollapseTelemetry::default();
        let buckets_pb = buckets
            .into_iter()
            .filter_map(|(timestamp, bucket)| {
                // Always keep `bufferLen` buckets (default is 2: current + previous one).
                // This is a trade-off: we accept slightly late traces (clock skew and stuff)
                // but we delay flushing by at most `bufferLen` buckets.
                // This delay might result in not flushing stats payload (data loss)
                // if the tracer stops while the latest buckets aren't old enough to be flushed.
                // The "force" boolean skips the delay and flushes all buckets, typically on
                // shutdown.
                let keep = !force
                    && timestamp > (now_timestamp - self.buffer_len as u64 * self.bucket_size);
                if keep {
                    self.buckets.insert(timestamp, bucket);
                    return None;
                }
                total_collapsed += bucket.collapsed_counts();
                #[cfg(feature = "stats-obfuscation")]
                let obfuscated = bucket.obfuscated;
                #[cfg(not(feature = "stats-obfuscation"))]
                let obfuscated = false;
                Some((obfuscated, encode(bucket, self.bucket_size)))
            })
            .collect();

        if !self.cardinality_log_emitted && total_collapsed.whole_key > 0 {
            self.cardinality_log_emitted = true;
            debug!(
                max_entries_per_bucket = self.cardinality_limits.whole_key_limit,
                total_whole_key_collapsed = total_collapsed.whole_key,
                "Client-side stats values have been collapsed to 'tracer_blocked_value'. This is due to the cardinality exceeding DD_TRACE_STATS_CARDINALITY_LIMIT"
            );
        }
        if !self.cardinality_log_emitted
            && (total_collapsed.resources > 0
                || total_collapsed.http_endpoint > 0
                || total_collapsed.peer_tags > 0
                || total_collapsed.additional_tags > 0)
        {
            self.cardinality_log_emitted = true;
            debug!(
                max_distinct_resource_per_bucket = self.cardinality_limits.resource_limit,
                total_resource_collapsed = total_collapsed.resources,
                max_distinct_http_endpoint_per_bucket = self.cardinality_limits.http_endpoint_limit,
                total_http_endpoint_collapsed = total_collapsed.http_endpoint,
                max_distinct_peer_tags_per_bucket = self.cardinality_limits.peer_tags_limit,
                total_peer_tags_collapsed = total_collapsed.peer_tags,
                max_distinct_additional_tags_per_bucket = self.cardinality_limits.additional_tags_limit,
                total_additional_tags_collapsed = total_collapsed.additional_tags,
                "Client-side stats field have been collapsed to 'tracer_blocked_value'. This is due to the cardinality exceeding one of the DD_TRACE_STATS_*_CARDINALITY_LIMIT"
            );
        }

        (buckets_pb, total_collapsed)
    }
}

#[cfg(feature = "stats-obfuscation")]
impl StatsComputationObfuscationConfig {
    pub fn disabled() -> SharedStatsComputationObfuscationConfig {
        use arc_swap::ArcSwap;
        use std::sync::Arc;

        Arc::new(ArcSwap::from_pointee(
            StatsComputationObfuscationConfig::default(),
        ))
    }
}

#[cfg(test)]
mod tests;
