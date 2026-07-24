// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! Reusable EVP flagevaluation payload, coalescing, serialization, and sender
//! primitives for the `flageval-worker` ingestion schema.
//!
//! Two-tier aggregation (full → degraded → drop-counted), context pruning,
//! payload-limit degradation, JSON POST encoding, and Agent EVP proxy sending
//! live here so native FFE consumers can share the same behavior independent of
//! sidecar dispatch.
//!
//! Serialization note (bincode wire vs EVP POST): these types cross the
//! worker→sidecar IPC boundary, which is encoded with **bincode** — a
//! non-self-describing format whose derived `Deserialize` reads every field in
//! declaration order. `#[serde(skip_serializing_if = ...)]` is therefore
//! **incompatible** with the bincode wire: a skipped field is omitted on
//! serialize but still expected on deserialize, causing field misalignment and
//! a dropped batch. For that reason **all fields here are always serialized**
//! (no `skip_serializing_if`). The flageval-worker EVP schema rejects null /
//! empty placeholders (especially for degraded-tier events), so
//! [`encode_flag_evaluation_payloads`] strips null / empty placeholder entries
//! from the JSON before the HTTP POST, reproducing the old skip semantics only
//! on the outbound wire. `#[serde(default)]` is kept on fields that have it for
//! deserialize robustness.

use super::FfeTelemetryContext;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap};
use std::hash::Hash;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

mod sender;
pub use sender::{
    flagevaluation_agent_proxy_endpoint, send_flag_evaluation_batch, FlagEvaluationEvpSendConfig,
    EVP_FLAGEVALUATION_PATH, EVP_PAYLOAD_SIZE_LIMIT, EVP_SUBDOMAIN_HEADER, EVP_SUBDOMAIN_VALUE,
};

// ── Aggregation caps ────────────────────────────────────────────────────────
pub const EVAL_SCALE_TARGET_FLAGS: usize = 2_500;
pub const EVAL_SCALE_FULL_BUCKETS_PER_FLAG: usize = 50;
pub const EVAL_SCALE_USERS_PER_FLAG: usize = 1_000;
pub const EVAL_SCALE_PER_FLAG_HEADROOM_MULTIPLIER: usize = 10;
pub const EVAL_SCALE_DEGRADED_BUCKETS_PER_FLAG: usize = 10;
pub const EVAL_SCALE_FULL_BUCKET_TARGET: usize =
    EVAL_SCALE_TARGET_FLAGS * EVAL_SCALE_FULL_BUCKETS_PER_FLAG;
pub const EVAL_SCALE_PER_FLAG_BUCKET_TARGET: usize =
    EVAL_SCALE_PER_FLAG_HEADROOM_MULTIPLIER * EVAL_SCALE_USERS_PER_FLAG;
pub const EVAL_SCALE_DEGRADED_BUCKET_TARGET: usize =
    EVAL_SCALE_TARGET_FLAGS * EVAL_SCALE_DEGRADED_BUCKETS_PER_FLAG;
/// Maximum number of distinct full-tier buckets across all flags.
pub const GLOBAL_CAP: usize = 131_072;
/// Maximum number of full-tier buckets for a single flag.
pub const PER_FLAG_CAP: usize = EVAL_SCALE_PER_FLAG_BUCKET_TARGET;
/// Maximum number of distinct degraded-tier buckets across all flags.
pub const DEGRADED_CAP: usize = 32_768;
/// Maximum number of aggregated rows to include in one EVP POST body.
pub const MAX_EVENTS_PER_POST: usize = 512;

// ── Context pruning bounds ───────────────────────────────────────────────────
/// Maximum number of context fields to include in a full-tier event.
pub const MAX_CONTEXT_FIELDS: usize = 256;
/// Maximum byte length of a context field value string. Values exceeding this
/// are skipped entirely (not truncated) to avoid partial-data misattribution.
pub const MAX_FIELD_LENGTH: usize = 256;
/// Maximum nested context path depth accepted from FFI callers. A scalar at
/// `a.b.c.d` is retained; fields nested below that depth are skipped.
pub const MAX_CONTEXT_DEPTH: usize = 4;

// ── Top-level batch ──────────────────────────────────────────────────────────

/// Batch wrapper for EVP flagevaluation events.
///
/// Serializes to:
/// ```json
/// { "context": { "service": "…", "env": "…", "version": "…" },
///   "flagEvaluations": [ … ] }
/// ```
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct FfeFlagEvaluationBatch {
    pub context: FfeTelemetryContext,
    #[serde(rename = "flagEvaluations")]
    pub flag_evaluations: Vec<FfeFlagEvaluationEvent>,
}

// ── Per-event payload ────────────────────────────────────────────────────────

/// A single aggregated flag evaluation event.
///
/// **All fields are always serialized** (no `skip_serializing_if`) so the type
/// is safe over the non-self-describing bincode IPC wire (see the module-level
/// serialization note). The degraded tier therefore serializes optional fields
/// as `null`/`false` on the wire; the EVP payload encoder
/// ([`encode_flag_evaluation_payloads`]) strips those null/empty placeholders
/// before the EVP POST so the flageval-worker schema sees no null placeholders.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct FfeFlagEvaluationEvent {
    /// Unix timestamp of the aggregation window (milliseconds).
    pub timestamp: i64,
    /// Required: the flag key.
    pub flag: FlagKey,
    /// Earliest evaluation in this bucket (milliseconds since epoch).
    pub first_evaluation: i64,
    /// Latest evaluation in this bucket (milliseconds since epoch).
    pub last_evaluation: i64,
    /// Number of evaluations folded into this bucket.
    pub evaluation_count: u64,

    // Optional fields — present in the full tier, `None` in the degraded tier.
    // Serialized as `null` on the bincode wire; the flusher strips them.
    /// Variant key; absent when the evaluation returned the runtime default
    /// (no variant assigned).
    #[serde(default)]
    pub variant: Option<VariantKey>,
    /// Allocation key from the UFC rule that produced this evaluation.
    #[serde(default)]
    pub allocation: Option<AllocationKey>,
    /// Targeting rule key from UFC metadata. Omit when no real rule metadata exists.
    #[serde(default)]
    pub targeting_rule: Option<TargetingRuleKey>,
    /// Targeting key identifying the evaluation subject.
    #[serde(default)]
    pub targeting_key: Option<String>,
    /// Pruned evaluation context (≤256 fields, values ≤256 chars, skip-not-truncate).
    #[serde(default)]
    pub context: Option<FlagEvalEventContext>,
    /// Evaluation error, if any.
    #[serde(default)]
    pub error: Option<EvalError>,

    // Optional field — may appear in either tier.
    /// `true` when the evaluation returned the SDK runtime default (absent
    /// variant, not a UFC-assigned variant). Serialized as `false` on the wire
    /// when unset; the flusher strips the `false` placeholder before the POST.
    /// `#[serde(default)]` keeps deserialization robust when the field is absent.
    #[serde(default)]
    pub runtime_default_used: bool,
}

// ── Field sub-types ──────────────────────────────────────────────────────────

/// Holds the flag key for the `flag` field.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct FlagKey {
    pub key: String,
}

/// Holds the variant key for the `variant` field.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct VariantKey {
    pub key: String,
}

/// Holds the allocation key for the `allocation` field.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct AllocationKey {
    pub key: String,
}

/// Holds the targeting rule key for the `targeting_rule` field.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct TargetingRuleKey {
    pub key: String,
}

/// Holds the error message for the `error` field.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct EvalError {
    pub message: String,
}

/// Per-event context object.
///
/// `evaluation` carries the pruned context attributes; `dd.service` carries the
/// originating service name for cross-service attribution.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct FlagEvalEventContext {
    /// Pruned evaluation context attributes (≤256 fields, values ≤256 chars),
    /// carried over the wire as a **JSON-object string** (e.g. `{"plan":"premium"}`).
    ///
    /// The sidecar IPC codec is bincode, which cannot (de)serialize
    /// `serde_json::Value` (it relies on `deserialize_any`, which bincode
    /// rejects). To keep the bincode wire encodable, the pruned context is
    /// stringified at event-build time and re-expanded into a JSON object by the
    /// shared EVP encoder ([`encode_flag_evaluation_payloads`]) before the EVP
    /// POST, so the on-the-wire EVP schema (`context.evaluation` as an object)
    /// is unchanged. `Eq` is preserved because `String` is `Eq`.
    ///
    /// Always serialized (no `skip_serializing_if`) for bincode-wire safety;
    /// the EVP payload encoder strips it when `None` → `null`.
    #[serde(default)]
    pub evaluation: Option<String>,
    /// Datadog-specific context sub-object. Always serialized for bincode-wire
    /// safety; the flusher strips it when `None` → `null` (and recursively
    /// removes the enclosing `context` object if it becomes empty).
    #[serde(default)]
    pub dd: Option<ContextDD>,
}

/// Datadog-specific context fields inside the per-event `context.dd` object.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ContextDD {
    /// Originating service name. Always serialized for bincode-wire safety; the
    /// flusher strips it when empty (`""`), reproducing the old
    /// `skip_serializing_if = "String::is_empty"` semantics on the POST.
    #[serde(default)]
    pub service: String,
}

// ── Context pruning ──────────────────────────────────────────────────────────

/// Prune evaluation context attributes to satisfy the flagevaluation bounds:
/// - At most `MAX_CONTEXT_FIELDS` (256) entries are kept.
/// - String values longer than `MAX_FIELD_LENGTH` (256 chars) are **skipped** (not truncated) to
///   avoid partial-data misattribution.
/// - Non-string values (bool, number, null) are kept regardless of their display length.
/// - Keys are iterated in sorted order for deterministic canonical-key stability; the returned
///   `BTreeMap` preserves that order.
pub fn prune_context(
    attrs: &BTreeMap<String, serde_json::Value>,
) -> BTreeMap<String, serde_json::Value> {
    attrs
        .iter()
        .filter(|(_, v)| {
            // Skip string values that exceed the per-field byte limit.
            if let serde_json::Value::String(s) = v {
                s.len() <= MAX_FIELD_LENGTH
            } else {
                true
            }
        })
        .take(MAX_CONTEXT_FIELDS)
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect()
}

#[derive(Default)]
pub struct FlagEvaluationEvpPayloadBuildResult {
    pub payloads: Vec<String>,
    pub dropped_oversized_rows: u64,
    pub degraded_oversized_rows: u64,
    pub payload_splits: u64,
}

#[derive(Default)]
pub struct FlagEvaluationEvpWriterStats {
    pub rows_dropped_degraded_cap: u64,
    pub rows_dropped_payload_limit: u64,
    pub rows_degraded_cardinality_cap: u64,
    pub rows_degraded_payload_limit: u64,
    pub payload_splits: u64,
}

#[derive(Default)]
struct FlagEvaluationEvpWriterCounters {
    rows_dropped_degraded_cap: AtomicU64,
    rows_dropped_payload_limit: AtomicU64,
    rows_degraded_cardinality_cap: AtomicU64,
    rows_degraded_payload_limit: AtomicU64,
    payload_splits: AtomicU64,
}

impl FlagEvaluationEvpWriterCounters {
    fn add_rows_dropped_degraded_cap(&self, count: u64) {
        add_counter(&self.rows_dropped_degraded_cap, count);
    }

    fn add_rows_dropped_payload_limit(&self, count: u64) {
        add_counter(&self.rows_dropped_payload_limit, count);
    }

    fn add_rows_degraded_cardinality_cap(&self, count: u64) {
        add_counter(&self.rows_degraded_cardinality_cap, count);
    }

    fn add_rows_degraded_payload_limit(&self, count: u64) {
        add_counter(&self.rows_degraded_payload_limit, count);
    }

    fn add_payload_splits(&self, count: u64) {
        add_counter(&self.payload_splits, count);
    }

    fn collect_writer_stats(&self) -> FlagEvaluationEvpWriterStats {
        FlagEvaluationEvpWriterStats {
            rows_dropped_degraded_cap: self.rows_dropped_degraded_cap.swap(0, Ordering::Relaxed),
            rows_dropped_payload_limit: self.rows_dropped_payload_limit.swap(0, Ordering::Relaxed),
            rows_degraded_cardinality_cap: self
                .rows_degraded_cardinality_cap
                .swap(0, Ordering::Relaxed),
            rows_degraded_payload_limit: self
                .rows_degraded_payload_limit
                .swap(0, Ordering::Relaxed),
            payload_splits: self.payload_splits.swap(0, Ordering::Relaxed),
        }
    }
}

fn add_counter(counter: &AtomicU64, count: u64) {
    if count > 0 {
        counter.fetch_add(count, Ordering::Relaxed);
    }
}

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
struct EventKey {
    flag_key: String,
    variant_key: Option<String>,
    allocation_key: Option<String>,
    targeting_rule_key: Option<String>,
    targeting_key: Option<String>,
    context_evaluation: Option<String>,
    context_dd_service: Option<String>,
    error_message: Option<String>,
    runtime_default_used: bool,
}

impl EventKey {
    fn new(event: &FfeFlagEvaluationEvent) -> Self {
        Self {
            flag_key: event.flag.key.clone(),
            variant_key: event.variant.as_ref().map(|v| v.key.clone()),
            allocation_key: event.allocation.as_ref().map(|a| a.key.clone()),
            targeting_rule_key: event.targeting_rule.as_ref().map(|r| r.key.clone()),
            targeting_key: event.targeting_key.clone(),
            context_evaluation: event
                .context
                .as_ref()
                .and_then(|context| context.evaluation.clone()),
            context_dd_service: event
                .context
                .as_ref()
                .and_then(|context| context.dd.as_ref().map(|dd| dd.service.clone())),
            error_message: event.error.as_ref().map(|e| e.message.clone()),
            runtime_default_used: event.runtime_default_used,
        }
    }

    fn degraded(event: &FfeFlagEvaluationEvent) -> Self {
        Self {
            flag_key: event.flag.key.clone(),
            variant_key: event.variant.as_ref().map(|v| v.key.clone()),
            allocation_key: event.allocation.as_ref().map(|a| a.key.clone()),
            targeting_rule_key: event.targeting_rule.as_ref().map(|r| r.key.clone()),
            targeting_key: None,
            context_evaluation: None,
            context_dd_service: None,
            error_message: event.error.as_ref().map(|e| e.message.clone()),
            runtime_default_used: event.runtime_default_used,
        }
    }
}

struct PendingDestination<D> {
    destination: D,
    context: FfeTelemetryContext,
    events: HashMap<EventKey, FfeFlagEvaluationEvent>,
}

struct CoalescerState<D> {
    destinations: HashMap<D, PendingDestination<D>>,
    flush_running: bool,
    pending_bucket_count: usize,
    full_bucket_count: usize,
    full_bucket_count_by_flag: HashMap<String, usize>,
    degraded_bucket_count: usize,
    dropped_overflow: u64,
}

// Keep this manual: deriving `Default` adds an unnecessary `D: Default` bound,
// but the empty state only needs default maps and counters.
impl<D> Default for CoalescerState<D> {
    fn default() -> Self {
        Self {
            destinations: HashMap::new(),
            flush_running: false,
            pending_bucket_count: 0,
            full_bucket_count: 0,
            full_bucket_count_by_flag: HashMap::new(),
            degraded_bucket_count: 0,
            dropped_overflow: 0,
        }
    }
}

/// Shared flagevaluation coalescer.
///
/// The generic destination is owned by the transport adapter. For the sidecar it
/// is the EVP proxy endpoint plus batch context; an agentless sender can use
/// its own endpoint identity without changing aggregation semantics.
pub struct FlagEvaluationEvpCoalescer<D> {
    state: Arc<Mutex<CoalescerState<D>>>,
    writer_stats: Arc<FlagEvaluationEvpWriterCounters>,
}

impl<D> Clone for FlagEvaluationEvpCoalescer<D> {
    fn clone(&self) -> Self {
        Self {
            state: Arc::clone(&self.state),
            writer_stats: Arc::clone(&self.writer_stats),
        }
    }
}

// Keep this manual so callers can use destination key types without `Default`.
impl<D> Default for FlagEvaluationEvpCoalescer<D> {
    fn default() -> Self {
        Self {
            state: Arc::new(Mutex::new(CoalescerState::default())),
            writer_stats: Arc::new(FlagEvaluationEvpWriterCounters::default()),
        }
    }
}

impl<D> FlagEvaluationEvpCoalescer<D>
where
    D: Clone + Eq + Hash,
{
    /// Enqueue a batch and return whether the caller should start a flush loop.
    pub fn enqueue(&self, destination: D, batch: FfeFlagEvaluationBatch) -> bool {
        if batch.flag_evaluations.is_empty() {
            return false;
        }

        let mut state = lock_or_recover(&self.state);
        state
            .destinations
            .entry(destination.clone())
            .or_insert_with(|| PendingDestination {
                destination: destination.clone(),
                context: batch.context,
                events: HashMap::new(),
            });

        for mut event in batch.flag_evaluations {
            let key = EventKey::new(&event);
            if merge_pending_event(&mut state, &destination, &key, &event) {
                continue;
            }

            let flag_key = event.flag.key.clone();
            let full_bucket_count_for_flag = state
                .full_bucket_count_by_flag
                .get(&flag_key)
                .copied()
                .unwrap_or(0);

            if state.full_bucket_count < GLOBAL_CAP && full_bucket_count_for_flag < PER_FLAG_CAP {
                if insert_pending_event(&mut state, &destination, key, event) {
                    state.full_bucket_count += 1;
                    *state.full_bucket_count_by_flag.entry(flag_key).or_default() += 1;
                }
                continue;
            }

            event.targeting_key = None;
            event.context = None;
            let evaluation_count = event.evaluation_count;
            let degraded_key = EventKey::degraded(&event);
            if merge_pending_event(&mut state, &destination, &degraded_key, &event) {
                self.writer_stats
                    .add_rows_degraded_cardinality_cap(evaluation_count);
                continue;
            }

            if state.degraded_bucket_count >= DEGRADED_CAP
                || state.pending_bucket_count >= GLOBAL_CAP + DEGRADED_CAP
            {
                state.dropped_overflow = state.dropped_overflow.saturating_add(evaluation_count);
                self.writer_stats
                    .add_rows_dropped_degraded_cap(evaluation_count);
                continue;
            }

            if insert_pending_event(&mut state, &destination, degraded_key, event) {
                state.degraded_bucket_count += 1;
                self.writer_stats
                    .add_rows_degraded_cardinality_cap(evaluation_count);
            }
        }

        if state.flush_running {
            false
        } else {
            state.flush_running = true;
            true
        }
    }

    pub fn take_batches(&self) -> Vec<(D, FfeFlagEvaluationBatch)> {
        let mut state = lock_or_recover(&self.state);
        if state.dropped_overflow > 0 {
            log::warn!(
                "ffe flagevaluation coalescer dropped {} pending bucket(s) after cardinality cap",
                state.dropped_overflow
            );
            state.dropped_overflow = 0;
        }

        let destinations = std::mem::take(&mut state.destinations);
        state.pending_bucket_count = 0;
        state.full_bucket_count = 0;
        state.full_bucket_count_by_flag.clear();
        state.degraded_bucket_count = 0;
        destinations
            .into_values()
            .filter_map(|pending| {
                if pending.events.is_empty() {
                    return None;
                }
                Some((
                    pending.destination,
                    FfeFlagEvaluationBatch {
                        context: pending.context,
                        flag_evaluations: pending.events.into_values().collect(),
                    },
                ))
            })
            .collect()
    }

    /// Return true when the caller's flush loop can stop.
    pub fn finish_flush_cycle(&self) -> bool {
        let mut state = lock_or_recover(&self.state);
        if state.destinations.is_empty() {
            state.flush_running = false;
            true
        } else {
            false
        }
    }

    pub fn collect_writer_stats(&self) -> FlagEvaluationEvpWriterStats {
        self.writer_stats.collect_writer_stats()
    }

    pub fn record_payload_build_result(&self, result: &FlagEvaluationEvpPayloadBuildResult) {
        self.writer_stats
            .add_rows_dropped_payload_limit(result.dropped_oversized_rows);
        self.writer_stats
            .add_rows_degraded_payload_limit(result.degraded_oversized_rows);
        self.writer_stats.add_payload_splits(result.payload_splits);
    }

    #[cfg(test)]
    fn force_bucket_counts_for_test(&self, full_bucket_count: usize, degraded_bucket_count: usize) {
        let mut state = lock_or_recover(&self.state);
        state.flush_running = true;
        state.full_bucket_count = full_bucket_count;
        state.degraded_bucket_count = degraded_bucket_count;
        state.pending_bucket_count = full_bucket_count.saturating_add(degraded_bucket_count);
    }
}

fn lock_or_recover<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(|e| e.into_inner())
}

fn merge_pending_event<D>(
    state: &mut CoalescerState<D>,
    destination: &D,
    key: &EventKey,
    event: &FfeFlagEvaluationEvent,
) -> bool
where
    D: Eq + Hash,
{
    let Some(pending) = state.destinations.get_mut(destination) else {
        return false;
    };

    if let Some(existing) = pending.events.get_mut(key) {
        merge_event(existing, event);
        true
    } else {
        false
    }
}

fn insert_pending_event<D>(
    state: &mut CoalescerState<D>,
    destination: &D,
    key: EventKey,
    event: FfeFlagEvaluationEvent,
) -> bool
where
    D: Eq + Hash,
{
    let Some(pending) = state.destinations.get_mut(destination) else {
        log::warn!("ffe flagevaluation coalescer missing pending destination; dropping event");
        return false;
    };

    pending.events.insert(key, event);
    state.pending_bucket_count += 1;
    true
}

fn merge_event(existing: &mut FfeFlagEvaluationEvent, incoming: &FfeFlagEvaluationEvent) {
    existing.timestamp = existing.timestamp.max(incoming.timestamp);
    existing.first_evaluation = existing.first_evaluation.min(incoming.first_evaluation);
    existing.last_evaluation = existing.last_evaluation.max(incoming.last_evaluation);
    existing.evaluation_count = existing
        .evaluation_count
        .saturating_add(incoming.evaluation_count);
}

pub fn encode_flag_evaluation_payloads(
    batch: FfeFlagEvaluationBatch,
    payload_size_limit: usize,
) -> Result<FlagEvaluationEvpPayloadBuildResult, serde_json::Error> {
    let FfeFlagEvaluationBatch {
        context,
        flag_evaluations,
    } = batch;

    let context_json = build_context_payload(&context)?;
    let payload_prefix = format!(r#"{{"context":{context_json},"flagEvaluations":["#);
    let payload_suffix = "]}";
    let base_payload_size = payload_prefix.len() + payload_suffix.len();

    let mut result = FlagEvaluationEvpPayloadBuildResult::default();
    let mut current_events = Vec::new();
    let mut current_size = base_payload_size;

    for event in flag_evaluations {
        if current_events.len() >= MAX_EVENTS_PER_POST {
            push_payload(
                &mut result.payloads,
                &payload_prefix,
                payload_suffix,
                &mut current_events,
            );
            current_size = base_payload_size;
        }

        let mut encoded_event = build_event_payload(&event)?;
        let mut event_size = encoded_event.len();
        if !single_event_fits(base_payload_size, event_size, payload_size_limit) {
            let Some(degraded_event) = degrade_event_for_payload_limit(&event) else {
                result.dropped_oversized_rows = result
                    .dropped_oversized_rows
                    .saturating_add(event.evaluation_count);
                continue;
            };

            let degraded_encoded_event = build_event_payload(&degraded_event)?;
            let degraded_event_size = degraded_encoded_event.len();
            if !single_event_fits(base_payload_size, degraded_event_size, payload_size_limit) {
                result.dropped_oversized_rows = result
                    .dropped_oversized_rows
                    .saturating_add(event.evaluation_count);
                continue;
            }

            encoded_event = degraded_encoded_event;
            event_size = degraded_event_size;
            result.degraded_oversized_rows = result
                .degraded_oversized_rows
                .saturating_add(degraded_event.evaluation_count);
        }

        let separator_size = usize::from(!current_events.is_empty());
        if current_size + separator_size + event_size > payload_size_limit
            && !current_events.is_empty()
        {
            push_payload(
                &mut result.payloads,
                &payload_prefix,
                payload_suffix,
                &mut current_events,
            );
            current_size = base_payload_size;
        }

        let separator_size = usize::from(!current_events.is_empty());
        current_size += separator_size + event_size;
        current_events.push(encoded_event);
    }

    if !current_events.is_empty() {
        push_payload(
            &mut result.payloads,
            &payload_prefix,
            payload_suffix,
            &mut current_events,
        );
    }
    result.payload_splits = result.payloads.len().saturating_sub(1) as u64;

    Ok(result)
}

fn single_event_fits(
    base_payload_size: usize,
    event_size: usize,
    payload_size_limit: usize,
) -> bool {
    base_payload_size.saturating_add(event_size) <= payload_size_limit
}

fn push_payload(
    payloads: &mut Vec<String>,
    payload_prefix: &str,
    payload_suffix: &str,
    encoded_events: &mut Vec<String>,
) {
    debug_assert!(
        !encoded_events.is_empty(),
        "callers should only push non-empty payload event groups"
    );
    if encoded_events.is_empty() {
        return;
    }

    let events_size: usize = encoded_events.iter().map(String::len).sum();
    let separators_size = encoded_events.len().saturating_sub(1);
    let mut payload = String::with_capacity(
        payload_prefix.len() + events_size + separators_size + payload_suffix.len(),
    );
    payload.push_str(payload_prefix);
    for (idx, event) in encoded_events.iter().enumerate() {
        if idx > 0 {
            payload.push(',');
        }
        payload.push_str(event);
    }
    payload.push_str(payload_suffix);
    payloads.push(payload);
    encoded_events.clear();
}

fn degrade_event_for_payload_limit(
    event: &FfeFlagEvaluationEvent,
) -> Option<FfeFlagEvaluationEvent> {
    if event.targeting_key.is_none() && event.context.is_none() {
        return None;
    }

    let mut degraded = event.clone();
    degraded.targeting_key = None;
    degraded.context = None;
    Some(degraded)
}

#[cfg(test)]
fn build_payload(batch: &FfeFlagEvaluationBatch) -> Result<String, serde_json::Error> {
    let context_json = build_context_payload(&batch.context)?;
    let mut encoded_events = Vec::with_capacity(batch.flag_evaluations.len());
    for event in &batch.flag_evaluations {
        encoded_events.push(build_event_payload(event)?);
    }

    Ok(build_payload_from_encoded_events(
        &context_json,
        &encoded_events,
    ))
}

fn build_context_payload(context: &FfeTelemetryContext) -> Result<String, serde_json::Error> {
    let mut value = serde_json::to_value(context)?;
    strip_placeholders(&mut value);
    serde_json::to_string(&value)
}

fn build_event_payload(event: &FfeFlagEvaluationEvent) -> Result<String, serde_json::Error> {
    let mut value = serde_json::to_value(event)?;
    expand_event_context(&mut value);
    strip_placeholders(&mut value);
    serde_json::to_string(&value)
}

#[cfg(test)]
fn build_payload_from_encoded_events(context_json: &str, encoded_events: &[String]) -> String {
    let events_size: usize = encoded_events.iter().map(String::len).sum();
    let separators_size = encoded_events.len().saturating_sub(1);
    let mut payload = String::with_capacity(
        r#"{"context":"#.len()
            + context_json.len()
            + r#","flagEvaluations":["#.len()
            + events_size
            + separators_size
            + "]}".len(),
    );

    payload.push_str(r#"{"context":"#);
    payload.push_str(context_json);
    payload.push_str(r#","flagEvaluations":["#);
    for (idx, event) in encoded_events.iter().enumerate() {
        if idx > 0 {
            payload.push(',');
        }
        payload.push_str(event);
    }
    payload.push_str("]}");
    payload
}

fn expand_event_context(event: &mut serde_json::Value) {
    let Some(context) = event.get_mut("context") else {
        return;
    };
    let Some(evaluation) = context.get_mut("evaluation") else {
        return;
    };
    let Some(s) = evaluation.as_str() else {
        return;
    };

    match serde_json::from_str::<serde_json::Value>(s) {
        Ok(parsed) => *evaluation = parsed,
        Err(_) => {
            if let Some(obj) = context.as_object_mut() {
                obj.remove("evaluation");
            }
        }
    }
}

fn strip_placeholders(value: &mut serde_json::Value) {
    strip_placeholders_at(value, PlaceholderLocation::Root);
}

#[derive(Clone, Copy, Eq, PartialEq)]
enum PlaceholderLocation {
    Root,
    RootContext,
    Other,
}

fn strip_placeholders_at(value: &mut serde_json::Value, location: PlaceholderLocation) {
    match value {
        serde_json::Value::Object(map) => {
            for (key, child) in map.iter_mut() {
                if !(location == PlaceholderLocation::RootContext && key == "evaluation") {
                    let child_location =
                        if location == PlaceholderLocation::Root && key == "context" {
                            PlaceholderLocation::RootContext
                        } else {
                            PlaceholderLocation::Other
                        };
                    strip_placeholders_at(child, child_location);
                }
            }
            map.retain(|key, v| !is_placeholder(key, v));
        }
        serde_json::Value::Array(items) => {
            for item in items.iter_mut() {
                strip_placeholders_at(item, PlaceholderLocation::Other);
            }
            items.retain(|v| !is_array_placeholder(v));
        }
        _ => {}
    }
}

fn is_placeholder(key: &str, value: &serde_json::Value) -> bool {
    match value {
        serde_json::Value::Null => true,
        serde_json::Value::Bool(b) => key == "runtime_default_used" && !b,
        serde_json::Value::String(s) => {
            matches!(key, "service" | "env" | "version") && s.is_empty()
        }
        serde_json::Value::Object(map) => map.is_empty(),
        serde_json::Value::Array(items) => items.is_empty(),
        serde_json::Value::Number(_) => false,
    }
}

fn is_array_placeholder(value: &serde_json::Value) -> bool {
    match value {
        serde_json::Value::Null => true,
        serde_json::Value::Object(map) => map.is_empty(),
        serde_json::Value::Array(items) => items.is_empty(),
        _ => false,
    }
}

// ── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{json, Value};

    fn context() -> FfeTelemetryContext {
        FfeTelemetryContext {
            service: "svc".to_owned(),
            env: "prod".to_owned(),
            version: "1".to_owned(),
        }
    }

    fn full_event() -> FfeFlagEvaluationEvent {
        FfeFlagEvaluationEvent {
            timestamp: 1_700_000_000_000,
            flag: FlagKey {
                key: "my-flag".to_owned(),
            },
            first_evaluation: 1_699_999_990_000,
            last_evaluation: 1_700_000_000_000,
            evaluation_count: 42,
            variant: Some(VariantKey {
                key: "on".to_owned(),
            }),
            allocation: Some(AllocationKey {
                key: "alloc-a".to_owned(),
            }),
            targeting_key: Some("user-123".to_owned()),
            targeting_rule: None,
            context: Some(FlagEvalEventContext {
                evaluation: Some(
                    serde_json::to_string(&{
                        let mut m = BTreeMap::new();
                        m.insert("plan".to_owned(), json!("premium"));
                        m
                    })
                    .unwrap(),
                ),
                dd: Some(ContextDD {
                    service: "frontend".to_owned(),
                }),
            }),
            error: None,
            runtime_default_used: false,
        }
    }

    // ── Test: required fields present in serialized JSON ──────────────────────

    #[test]
    fn fully_populated_event_serializes_required_fields() {
        let batch = FfeFlagEvaluationBatch {
            context: context(),
            flag_evaluations: vec![full_event()],
        };
        let json = serde_json::to_string(&batch).unwrap();
        let v: Value = serde_json::from_str(&json).unwrap();

        assert_eq!(v["context"]["service"], "svc");
        assert_eq!(v["context"]["env"], "prod");
        assert_eq!(v["context"]["version"], "1");

        let ev = &v["flagEvaluations"][0];
        assert_eq!(ev["flag"]["key"], "my-flag");
        assert!(ev["first_evaluation"].is_number());
        assert!(ev["last_evaluation"].is_number());
        assert_eq!(ev["evaluation_count"], 42);
        assert_eq!(ev["variant"]["key"], "on");
        assert_eq!(ev["allocation"]["key"], "alloc-a");
        assert_eq!(ev["targeting_key"], "user-123");
    }

    fn degraded_event() -> FfeFlagEvaluationEvent {
        FfeFlagEvaluationEvent {
            timestamp: 1_700_000_000_000,
            flag: FlagKey {
                key: "flag-b".to_owned(),
            },
            first_evaluation: 1_699_999_990_000,
            last_evaluation: 1_700_000_000_000,
            evaluation_count: 7,
            variant: None,
            allocation: None,
            targeting_rule: None,
            targeting_key: None,
            context: None,
            error: None,
            runtime_default_used: false,
        }
    }

    // ── Test: degraded-tier event serializes optional fields as null ──────────
    //
    // The type does not use `skip_serializing_if` (bincode-wire safety), so on
    // the wire `None`/`false` optional fields ARE present (as null/false). The
    // null-placeholder stripping that the flageval-worker schema requires
    // happens in the EVP payload encoder, not at the type level.

    #[test]
    fn degraded_tier_event_serializes_optional_fields_as_null() {
        let degraded = degraded_event();
        let json = serde_json::to_string(&degraded).unwrap();
        let v: Value = serde_json::from_str(&json).unwrap();

        // Required fields present.
        assert_eq!(v["flag"]["key"], "flag-b");
        assert!(v["first_evaluation"].is_number());
        assert!(v["last_evaluation"].is_number());
        assert_eq!(v["evaluation_count"], 7);

        // Optional fields are present as null/false placeholders on the wire
        // (stripped later by the flusher, not at the type level).
        assert!(v["variant"].is_null(), "variant should serialize as null");
        assert!(
            v["allocation"].is_null(),
            "allocation should serialize as null"
        );
        assert!(
            v["targeting_rule"].is_null(),
            "targeting_rule should serialize as null"
        );
        assert!(
            v["targeting_key"].is_null(),
            "targeting_key should serialize as null"
        );
        assert!(v["context"].is_null(), "context should serialize as null");
        assert!(v["error"].is_null(), "error should serialize as null");
        assert_eq!(
            v["runtime_default_used"], false,
            "runtime_default_used should serialize as false"
        );
    }

    #[test]
    fn cap_sizing_constants_preserve_default_caps() {
        assert_eq!(EVAL_SCALE_FULL_BUCKET_TARGET, 125_000);
        assert_eq!(EVAL_SCALE_PER_FLAG_BUCKET_TARGET, 10_000);
        assert_eq!(EVAL_SCALE_DEGRADED_BUCKET_TARGET, 25_000);
        assert_eq!(GLOBAL_CAP, 131_072);
        assert_eq!(PER_FLAG_CAP, 10_000);
        assert_eq!(DEGRADED_CAP, 32_768);
    }

    // ── Test: bincode round-trip with mixed Some/None fields ──────────────────
    //
    // Mechanical guard for the worker→sidecar IPC bug: bincode is a
    // non-self-describing codec, so any `skip_serializing_if` on these types
    // would omit a field on serialize while derived Deserialize still expects it
    // in order → field misalignment → the sidecar drops the batch. A batch
    // mixing a full-tier event (Some fields) and degraded-tier event (None
    // fields) must survive serialize→deserialize byte-for-byte.

    #[test]
    fn batch_round_trips_via_bincode_with_mixed_optional_fields() {
        let batch = FfeFlagEvaluationBatch {
            context: context(),
            flag_evaluations: vec![full_event(), degraded_event()],
        };
        let bytes = bincode::serialize(&batch).expect("bincode serialize must succeed");
        let decoded: FfeFlagEvaluationBatch =
            bincode::deserialize(&bytes).expect("bincode deserialize must succeed");
        assert_eq!(
            batch, decoded,
            "bincode round-trip must be lossless for a batch mixing Some and None fields"
        );
    }

    // ── Test: context pruning — 256-field limit ───────────────────────────────

    #[test]
    fn context_pruning_keeps_at_most_256_fields() {
        let mut attrs = BTreeMap::new();
        for i in 0..300usize {
            attrs.insert(format!("key{i:04}"), json!(i.to_string()));
        }
        let pruned = prune_context(&attrs);
        assert_eq!(
            pruned.len(),
            MAX_CONTEXT_FIELDS,
            "pruned context must have at most {MAX_CONTEXT_FIELDS} fields"
        );
    }

    // ── Test: context pruning — skip string values > 256 chars ───────────────

    #[test]
    fn context_pruning_skips_oversized_string_values() {
        let mut attrs = BTreeMap::new();
        let long_value = "x".repeat(MAX_FIELD_LENGTH + 1);
        attrs.insert("oversized".to_owned(), json!(long_value));
        attrs.insert("ok".to_owned(), json!("short"));
        // Non-string values are kept regardless of length.
        attrs.insert("num".to_owned(), json!(12345));

        let pruned = prune_context(&attrs);
        assert!(
            !pruned.contains_key("oversized"),
            "oversized string value must be skipped"
        );
        assert!(pruned.contains_key("ok"), "short string value must be kept");
        assert!(
            pruned.contains_key("num"),
            "numeric value must be kept regardless of length"
        );
    }

    // ── Test: batch round-trips via serde ────────────────────────────────────

    #[test]
    fn batch_round_trips_via_serde() {
        let batch = FfeFlagEvaluationBatch {
            context: context(),
            flag_evaluations: vec![full_event()],
        };
        let json = serde_json::to_string(&batch).unwrap();
        let decoded: FfeFlagEvaluationBatch = serde_json::from_str(&json).unwrap();
        assert_eq!(batch, decoded);
    }

    #[test]
    fn payload_encoding_strips_degraded_tier_placeholders() {
        let batch = FfeFlagEvaluationBatch {
            context: context(),
            flag_evaluations: vec![degraded_event()],
        };
        let payload = build_payload(&batch).expect("build_payload must succeed");
        let v: Value = serde_json::from_str(&payload).unwrap();
        let ev = &v["flagEvaluations"][0];

        assert_eq!(ev["flag"]["key"], "flag-b");
        assert_eq!(ev["evaluation_count"], 7);
        assert!(ev["first_evaluation"].is_number());
        assert!(ev["last_evaluation"].is_number());
        assert!(ev["timestamp"].is_number());
        assert!(ev.get("variant").is_none(), "variant must be stripped");
        assert!(
            ev.get("allocation").is_none(),
            "allocation must be stripped"
        );
        assert!(
            ev.get("targeting_rule").is_none(),
            "targeting_rule must be stripped"
        );
        assert!(
            ev.get("targeting_key").is_none(),
            "targeting_key must be stripped"
        );
        assert!(ev.get("context").is_none(), "context must be stripped");
        assert!(ev.get("error").is_none(), "error must be stripped");
        assert!(
            ev.get("runtime_default_used").is_none(),
            "runtime_default_used=false must be stripped"
        );
    }

    #[test]
    fn payload_encoding_strips_empty_top_level_env_and_version_only_as_placeholders() {
        let batch = FfeFlagEvaluationBatch {
            context: FfeTelemetryContext {
                service: "svc".to_owned(),
                env: String::new(),
                version: String::new(),
            },
            flag_evaluations: vec![full_event()],
        };

        let payload = build_payload(&batch).expect("build_payload must succeed");
        let v: Value = serde_json::from_str(&payload).unwrap();

        assert_eq!(v["context"]["service"], "svc");
        assert!(
            v["context"].get("env").is_none(),
            "empty env must be omitted from the request context"
        );
        assert!(
            v["context"].get("version").is_none(),
            "empty version must be omitted from the request context"
        );
        assert!(
            v["context"].is_object(),
            "request context must remain a JSON object"
        );
    }

    #[test]
    fn payload_encoding_keeps_full_tier_fields() {
        let mut event = full_event();
        event.targeting_rule = Some(TargetingRuleKey {
            key: "rule-1".to_owned(),
        });
        event.error = Some(EvalError {
            message: "boom".to_owned(),
        });
        event.runtime_default_used = true;
        let batch = FfeFlagEvaluationBatch {
            context: context(),
            flag_evaluations: vec![event],
        };
        let payload = build_payload(&batch).expect("build_payload must succeed");
        let v: Value = serde_json::from_str(&payload).unwrap();
        let ev = &v["flagEvaluations"][0];

        assert_eq!(ev["variant"]["key"], "on", "variant must be kept");
        assert_eq!(
            ev["allocation"]["key"], "alloc-a",
            "allocation must be kept"
        );
        assert_eq!(
            ev["targeting_rule"]["key"], "rule-1",
            "targeting_rule must be kept"
        );
        assert_eq!(
            ev["targeting_key"], "user-123",
            "targeting_key must be kept"
        );
        assert_eq!(ev["error"]["message"], "boom", "error must be kept");
        assert_eq!(
            ev["runtime_default_used"], true,
            "runtime_default_used=true must be kept"
        );
        assert!(
            ev.get("reason").is_none(),
            "EVP payload must not emit top-level OpenFeature reason"
        );

        let ctx = &ev["context"];
        assert!(
            ctx["evaluation"].is_object(),
            "context.evaluation must be an object: {}",
            ctx["evaluation"]
        );
        assert_eq!(ctx["evaluation"]["plan"], "premium");
        assert_eq!(
            ctx["dd"]["service"], "frontend",
            "context.dd.service must be kept"
        );
    }

    #[test]
    fn payload_encoding_collapses_empty_nested_context() {
        let mut ev = degraded_event();
        ev.context = Some(FlagEvalEventContext {
            evaluation: None,
            dd: Some(ContextDD {
                service: String::new(),
            }),
        });
        let batch = FfeFlagEvaluationBatch {
            context: context(),
            flag_evaluations: vec![ev],
        };
        let payload = build_payload(&batch).expect("build_payload must succeed");
        let v: Value = serde_json::from_str(&payload).unwrap();

        assert!(
            v["flagEvaluations"][0].get("context").is_none(),
            "a context that becomes empty after cleaning must be removed entirely"
        );
    }

    #[test]
    fn payload_encoding_expands_evaluation_string_into_object() {
        let batch = FfeFlagEvaluationBatch {
            context: context(),
            flag_evaluations: vec![full_event()],
        };
        let payload = build_payload(&batch).expect("build_payload must succeed");
        let v: Value = serde_json::from_str(&payload).unwrap();

        let evaluation = &v["flagEvaluations"][0]["context"]["evaluation"];
        assert!(
            evaluation.is_object(),
            "context.evaluation must be a JSON object in the POST body, not a string: {evaluation}"
        );
        assert_eq!(
            evaluation["plan"], "premium",
            "the expanded object must preserve the original key/value"
        );
        assert!(
            !evaluation.is_string(),
            "context.evaluation must not remain a quoted string"
        );
    }

    #[test]
    fn payload_encoding_drops_unparseable_evaluation_gracefully() {
        let mut event = full_event();
        event.context = Some(FlagEvalEventContext {
            evaluation: Some("this is not json".to_owned()),
            dd: None,
        });
        let batch = FfeFlagEvaluationBatch {
            context: context(),
            flag_evaluations: vec![event],
        };

        let payload = build_payload(&batch).expect("build_payload must not fail on bad input");
        let v: Value = serde_json::from_str(&payload).unwrap();

        assert!(
            v["flagEvaluations"][0]["context"]
                .get("evaluation")
                .is_none(),
            "unparseable evaluation must be dropped from the body"
        );
    }

    #[test]
    fn placeholder_stripping_recurses_into_non_context_evaluation_objects() {
        let mut value = json!({
            "evaluation": {
                "empty_array": [],
                "empty_object": {},
                "items": [null, {}, [], "kept"],
                "null_value": null,
                "present": "kept"
            }
        });

        strip_placeholders(&mut value);

        assert_eq!(
            value,
            json!({
                "evaluation": {
                    "items": ["kept"],
                    "present": "kept"
                }
            })
        );
    }

    #[test]
    fn placeholder_stripping_preserves_context_evaluation_subtree() {
        let mut value = json!({
            "context": {
                "evaluation": {
                    "enabled": false,
                    "empty": "",
                    "empty_array": [],
                    "empty_object": {},
                    "null_value": null
                },
                "dd": {
                    "service": ""
                }
            }
        });

        strip_placeholders(&mut value);

        let evaluation = &value["context"]["evaluation"];
        assert_eq!(evaluation["enabled"], false);
        assert_eq!(evaluation["empty"], "");
        assert!(evaluation["empty_array"].is_array());
        assert!(evaluation["empty_object"].is_object());
        assert!(evaluation["null_value"].is_null());
        assert!(value["context"].get("dd").is_none());
    }

    #[test]
    fn payload_encoding_preserves_false_and_empty_context_values() {
        let mut event = full_event();
        event.context = Some(FlagEvalEventContext {
            evaluation: Some(
                json!({
                    "enabled": false,
                    "empty": "",
                    "empty_object": {},
                    "empty_array": []
                })
                .to_string(),
            ),
            dd: None,
        });
        let batch = FfeFlagEvaluationBatch {
            context: context(),
            flag_evaluations: vec![event],
        };

        let payload = build_payload(&batch).expect("build_payload must succeed");
        let v: Value = serde_json::from_str(&payload).unwrap();
        let evaluation = &v["flagEvaluations"][0]["context"]["evaluation"];

        assert_eq!(evaluation["enabled"], false);
        assert_eq!(evaluation["empty"], "");
        assert!(evaluation["empty_object"].is_object());
        assert!(evaluation["empty_array"].is_array());
    }

    #[test]
    fn payload_encoding_empty_batch_has_no_payloads_or_splits() {
        let result = encode_flag_evaluation_payloads(
            FfeFlagEvaluationBatch {
                context: context(),
                flag_evaluations: vec![],
            },
            EVP_PAYLOAD_SIZE_LIMIT,
        )
        .expect("payload build must succeed");

        assert!(result.payloads.is_empty());
        assert_eq!(result.payload_splits, 0);
        assert_eq!(result.dropped_oversized_rows, 0);
        assert_eq!(result.degraded_oversized_rows, 0);
    }

    #[test]
    fn payload_encoding_splits_by_encoded_byte_limit() {
        let event = full_event();
        let batch = FfeFlagEvaluationBatch {
            context: context(),
            flag_evaluations: vec![event.clone(), event.clone(), event.clone()],
        };
        let one_event_limit = build_payload(&FfeFlagEvaluationBatch {
            context: context(),
            flag_evaluations: vec![event],
        })
        .unwrap()
        .len();

        let result = encode_flag_evaluation_payloads(batch, one_event_limit)
            .expect("payload build must succeed");

        assert_eq!(result.dropped_oversized_rows, 0);
        assert_eq!(result.degraded_oversized_rows, 0);
        assert_eq!(result.payload_splits, result.payloads.len() as u64 - 1);
        assert_eq!(
            result.payloads.len(),
            3,
            "the byte limit should split before a second event is appended"
        );
        for payload in &result.payloads {
            assert!(
                payload.len() <= one_event_limit,
                "payload length {} exceeded limit {}: {}",
                payload.len(),
                one_event_limit,
                payload
            );
        }
    }

    #[test]
    fn payload_encoding_degrades_oversized_full_event_before_drop() {
        let mut oversized = full_event();
        oversized.targeting_rule = Some(TargetingRuleKey {
            key: "rule-1".to_owned(),
        });
        oversized.error = Some(EvalError {
            message: "boom".to_owned(),
        });
        oversized.runtime_default_used = true;
        oversized.context = Some(FlagEvalEventContext {
            evaluation: Some(json!({ "blob": "x".repeat(1024) }).to_string()),
            dd: Some(ContextDD {
                service: "frontend".to_owned(),
            }),
        });

        let degraded = degrade_event_for_payload_limit(&oversized)
            .expect("full event should have a degraded form");
        let degraded_limit = build_payload(&FfeFlagEvaluationBatch {
            context: context(),
            flag_evaluations: vec![degraded],
        })
        .unwrap()
        .len();
        let full_size = build_payload(&FfeFlagEvaluationBatch {
            context: context(),
            flag_evaluations: vec![oversized.clone()],
        })
        .unwrap()
        .len();
        assert!(
            full_size > degraded_limit,
            "test setup must make the full event exceed the degraded limit"
        );

        let result = encode_flag_evaluation_payloads(
            FfeFlagEvaluationBatch {
                context: context(),
                flag_evaluations: vec![oversized],
            },
            degraded_limit,
        )
        .expect("payload build must succeed");

        assert_eq!(result.dropped_oversized_rows, 0);
        assert_eq!(result.degraded_oversized_rows, 42);
        assert_eq!(result.payload_splits, 0);
        assert_eq!(result.payloads.len(), 1);
        assert!(result.payloads[0].len() <= degraded_limit);

        let v: Value = serde_json::from_str(&result.payloads[0]).unwrap();
        let ev = &v["flagEvaluations"][0];
        assert!(
            ev.get("targeting_key").is_none(),
            "oversized full row must omit targeting_key after degradation"
        );
        assert!(
            ev.get("context").is_none(),
            "oversized full row must omit context after degradation"
        );
        assert_eq!(ev["variant"]["key"], "on");
        assert_eq!(ev["allocation"]["key"], "alloc-a");
        assert_eq!(ev["targeting_rule"]["key"], "rule-1");
        assert_eq!(ev["error"]["message"], "boom");
    }

    #[test]
    fn payload_encoding_drops_oversized_degraded_event() {
        let mut oversized = degraded_event();
        oversized.flag.key = "x".repeat(1024);

        let result = encode_flag_evaluation_payloads(
            FfeFlagEvaluationBatch {
                context: context(),
                flag_evaluations: vec![oversized],
            },
            128,
        )
        .expect("payload build must succeed");

        assert!(result.payloads.is_empty());
        assert_eq!(result.dropped_oversized_rows, 7);
        assert_eq!(result.degraded_oversized_rows, 0);
        assert_eq!(result.payload_splits, 0);
    }

    #[test]
    fn coalescer_coalesces_identical_batches() {
        let coalescer = FlagEvaluationEvpCoalescer::<String>::default();
        let destination = "agent".to_owned();

        assert!(coalescer.enqueue(
            destination.clone(),
            FfeFlagEvaluationBatch {
                context: context(),
                flag_evaluations: vec![full_event()],
            },
        ));
        assert!(!coalescer.enqueue(
            destination,
            FfeFlagEvaluationBatch {
                context: context(),
                flag_evaluations: vec![full_event()],
            },
        ));

        let batches = coalescer.take_batches();
        assert_eq!(batches.len(), 1);
        assert_eq!(batches[0].1.flag_evaluations.len(), 1);
        assert_eq!(batches[0].1.flag_evaluations[0].evaluation_count, 84);
        assert!(coalescer.finish_flush_cycle());
    }

    #[test]
    fn coalescer_degrades_after_per_flag_cap() {
        let coalescer = FlagEvaluationEvpCoalescer::<String>::default();
        let mut events = Vec::with_capacity(PER_FLAG_CAP + 50);
        for index in 0..(PER_FLAG_CAP + 50) {
            let mut event = full_event();
            event.evaluation_count = 1;
            event.targeting_key = Some(format!("user-{index}"));
            event.targeting_rule = Some(TargetingRuleKey {
                key: "rule-1".to_owned(),
            });
            events.push(event);
        }

        assert!(coalescer.enqueue(
            "agent".to_owned(),
            FfeFlagEvaluationBatch {
                context: context(),
                flag_evaluations: events,
            },
        ));

        let batches = coalescer.take_batches();
        assert_eq!(batches.len(), 1);
        let events = &batches[0].1.flag_evaluations;
        let full_events = events
            .iter()
            .filter(|event| event.targeting_key.is_some() || event.context.is_some())
            .count();
        let degraded = events
            .iter()
            .find(|event| event.targeting_key.is_none() && event.context.is_none())
            .expect("overflow must be folded into a degraded bucket");

        assert_eq!(full_events, PER_FLAG_CAP);
        assert_eq!(degraded.evaluation_count, 50);
        assert_eq!(
            degraded.variant.as_ref().map(|v| v.key.as_str()),
            Some("on")
        );
        assert_eq!(
            degraded.allocation.as_ref().map(|a| a.key.as_str()),
            Some("alloc-a")
        );
        assert_eq!(
            degraded
                .targeting_rule
                .as_ref()
                .map(|rule| rule.key.as_str()),
            Some("rule-1")
        );

        let stats = coalescer.collect_writer_stats();
        assert_eq!(stats.rows_degraded_cardinality_cap, 50);
        assert_eq!(stats.rows_dropped_degraded_cap, 0);
    }

    #[test]
    fn coalescer_counts_degraded_cap_drops_by_evaluation_count() {
        let coalescer = FlagEvaluationEvpCoalescer::<String>::default();
        coalescer.force_bucket_counts_for_test(GLOBAL_CAP, DEGRADED_CAP);
        let mut event = full_event();
        event.evaluation_count = 9;

        assert!(!coalescer.enqueue(
            "agent".to_owned(),
            FfeFlagEvaluationBatch {
                context: context(),
                flag_evaluations: vec![event],
            },
        ));

        let stats = coalescer.collect_writer_stats();
        assert_eq!(stats.rows_dropped_degraded_cap, 9);
        assert_eq!(stats.rows_degraded_cardinality_cap, 0);
    }
}
