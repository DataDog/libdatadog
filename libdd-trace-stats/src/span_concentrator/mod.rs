// Copyright 2024-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0
//! This module implements the SpanConcentrator used to aggregate spans into stats
use std::collections::HashMap;
use std::time::Duration;
use tracing::debug;
// std::time::SystemTime panics on wasm32.
use web_time::{SystemTime, UNIX_EPOCH};

use libdd_trace_protobuf::pb;
use tracing::warn;

use aggregation::StatsBucket;

mod aggregation;
use aggregation::BorrowedAggregationKey;
pub use aggregation::{FixedAggregationKey, OtlpExactCell, OtlpExactGroup, OtlpStatsBucket};

pub mod stat_span;
pub use stat_span::StatSpan;

const ADDITIONAL_METRIC_TAGS_MAX_KEYS: usize = 4;

/// Deduplicate, sort alphabetically, and cap `keys` using [`ADDITIONAL_METRIC_TAGS_MAX_KEYS`].
/// Excess keys are dropped and logged as a one time warning.
fn normalize_additional_metric_tag_keys(mut keys: Vec<String>) -> Vec<String> {
    keys.sort_unstable();
    keys.dedup();
    if keys.len() > ADDITIONAL_METRIC_TAGS_MAX_KEYS {
        let dropped = keys.split_off(ADDITIONAL_METRIC_TAGS_MAX_KEYS);
        warn!(
            "additional_metric_tag_keys: {} additional metric tag keys exceed the cap of {}; dropping: {:?}",
            dropped.len() + ADDITIONAL_METRIC_TAGS_MAX_KEYS,
            ADDITIONAL_METRIC_TAGS_MAX_KEYS,
            dropped,
        );
    }
    keys
}

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
    /// cardinality limiting across all flushed time buckets.
    pub collapsed_spans: u64,
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
    t.duration_since(UNIX_EPOCH)
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
    /// Maximum number of distinct aggregation keys per bucket.
    max_entries_per_bucket: usize,
    /// span.kind fields eligible for stats computation
    span_kinds_stats_computed: Vec<String>,
    /// keys for supplementary tags that describe peer.service entities
    peer_tag_keys: Vec<String>,
    /// keys for additional tags on trace stats
    additional_metric_tag_keys: Vec<String>,
    /// If true, the maximum length for a resource goes from `5_000` to `15_000`
    big_resource: bool,
    #[cfg(feature = "stats-obfuscation")]
    obfuscation_config: SharedStatsComputationObfuscationConfig,
}

impl SpanConcentrator {
    /// Return a new concentrator with the given parameters
    /// - `bucket_size` is the size of the time buckets
    /// - `now` the current system time, used to define the oldest bucket
    /// - `span_kinds_stats_computed` list of span kinds eligible for stats computation
    /// - `peer_tag_keys` list of keys considered as peer tags for aggregation
    /// - `override_max_entries_per_bucket` maximum distinct aggregation keys per time bucket before
    ///   cardinality limiting applies. Pass `None` to use [`DEFAULT_MAX_ENTRIES_PER_BUCKET`].
    /// - `additional_metric_tag_keys` list of keys considered as addtional tags for aggregation
    /// - `obfuscation_config` optional and updatable config for resource key obfuscation
    pub fn new(
        bucket_size: Duration,
        now: SystemTime,
        span_kinds_stats_computed: Vec<String>,
        peer_tag_keys: Vec<String>,
        override_max_entries_per_bucket: Option<usize>,
        additional_metric_tag_keys: Vec<String>,
        #[cfg(feature = "stats-obfuscation")] obfuscation_config: Option<
            SharedStatsComputationObfuscationConfig,
        >,
    ) -> SpanConcentrator {
        SpanConcentrator {
            bucket_size: bucket_size.as_nanos() as u64,
            buckets: HashMap::new(),
            oldest_timestamp: align_timestamp(
                system_time_to_unix_duration(now).as_nanos() as u64,
                bucket_size.as_nanos() as u64,
            ),
            buffer_len: 2,
            max_entries_per_bucket: override_max_entries_per_bucket
                .unwrap_or(DEFAULT_MAX_ENTRIES_PER_BUCKET),
            span_kinds_stats_computed,
            peer_tag_keys,
            additional_metric_tag_keys: normalize_additional_metric_tag_keys(
                additional_metric_tag_keys,
            ),
            big_resource: false,
            #[cfg(feature = "stats-obfuscation")]
            obfuscation_config: obfuscation_config.unwrap_or_default(),
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

    /// Set the `big_resource` flag, it changes the resource field limit from `5_000` to `15_000`
    pub fn set_big_resource(&mut self, big_resource: bool) {
        self.big_resource = big_resource;
    }

    /// Return the list of keys considered as additional_metric_tag_keys for aggregation
    pub fn additional_metric_tag_keys(&self) -> &[String] {
        &self.additional_metric_tag_keys
    }

    /// Set the list of keys considered as additional_metric_tag_keys for aggregation
    pub fn set_additional_metric_tag_keys(&mut self, tag_keys: Vec<String>) {
        self.additional_metric_tag_keys = normalize_additional_metric_tag_keys(tag_keys);
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
                self.max_entries_per_bucket,
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
                self.additional_metric_tag_keys.as_slice(),
            ),
            None => BorrowedAggregationKey::from_span(
                span,
                self.peer_tag_keys.as_slice(),
                self.additional_metric_tag_keys.as_slice(),
            ),
        };
        // Apply field truncation only when obfuscation was applied
        #[cfg(feature = "stats-obfuscation")]
        let mut agg_key = agg_key;
        #[cfg(feature = "stats-obfuscation")]
        if target_bucket.obfuscated {
            agg_key.truncate(self.big_resource);
        }
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
    ) -> (Vec<(bool, T)>, u64) {
        // TODO: Wait for HashMap::extract_if to be stabilized to avoid a full drain
        let now_timestamp = system_time_to_unix_duration(now).as_nanos() as u64;
        let buckets: Vec<(u64, StatsBucket)> = self.buckets.drain().collect();
        self.oldest_timestamp = if force {
            align_timestamp(now_timestamp, self.bucket_size)
        } else {
            align_timestamp(now_timestamp, self.bucket_size)
                - (self.buffer_len as u64 - 1) * self.bucket_size
        };
        let mut total_collapsed = 0;
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
                total_collapsed += bucket.collapsed_count();
                #[cfg(feature = "stats-obfuscation")]
                let obfuscated = bucket.obfuscated;
                #[cfg(not(feature = "stats-obfuscation"))]
                let obfuscated = false;
                Some((obfuscated, encode(bucket, self.bucket_size)))
            })
            .collect();
        if total_collapsed > 0 {
            debug!(max_entries_per_bucket = self.max_entries_per_bucket, total_collapsed, "Client-side stats values have been collapsed to 'tracer_blocked_value'. This is due to the cardinality exceeding DD_TRACE_STATS_CARDINALITY_LIMIT");
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
