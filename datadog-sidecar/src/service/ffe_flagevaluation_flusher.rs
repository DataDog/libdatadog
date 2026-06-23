// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! Serializes and forwards FFE (Feature Flag Evaluation) flag evaluation
//! batches to the Datadog Agent's EVP proxy.
//!
//! Protocol: `POST /evp_proxy/v2/api/v2/flagevaluation` with the header
//! `X-Datadog-EVP-Subdomain: event-platform-intake`. Fire-and-forget: non-2xx
//! responses are logged at `warn`, network errors at `debug`, and dropped
//! (matches dd-trace-go behaviour). No agent capability gate.

use crate::service::ffe_evp_proxy;
use crate::service::{FfeFlagEvaluationBatch, FfeFlagEvaluationEvent, FfeTelemetryContext};
use datadog_ffe::telemetry::flagevaluation::{DEGRADED_CAP, GLOBAL_CAP, PER_FLAG_CAP};
#[cfg(test)]
use ffe_evp_proxy::{EVP_SUBDOMAIN_HEADER, EVP_SUBDOMAIN_VALUE};
use libdd_capabilities::{HttpClientCapability, SleepCapability};
use libdd_capabilities_impl::NativeCapabilities;
use libdd_common::Endpoint;
use libdd_common::MutexExt;
use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tracing::{debug, warn};

/// EVP proxy path for FFE flag evaluation intake.
pub(crate) const EVP_FLAGEVALUATION_PATH: &str = "/evp_proxy/v2/api/v2/flagevaluation";

const LOG_PREFIX: &str = "ffe_flagevaluation_flusher";
const COALESCE_DELAY: Duration = Duration::from_millis(250);
const MAX_PENDING_BUCKETS: usize = GLOBAL_CAP + DEGRADED_CAP;
const MAX_EVENTS_PER_POST: usize = 512;
const EVP_PAYLOAD_SIZE_LIMIT: usize = 5 * 1024 * 1024;

pub(crate) const FLAG_EVALUATION_ROWS_DROPPED_METRIC: &str = "flagevaluation.rows.dropped";
pub(crate) const FLAG_EVALUATION_ROWS_DEGRADED_METRIC: &str = "flagevaluation.rows.degraded";
pub(crate) const FLAG_EVALUATION_PAYLOAD_SPLITS_METRIC: &str = "flagevaluation.payload.splits";

pub(crate) const FLAG_EVALUATION_REASON_DEGRADED_CAP: &str = "degraded_cap";
pub(crate) const FLAG_EVALUATION_REASON_CARDINALITY_CAP: &str = "cardinality_cap";
pub(crate) const FLAG_EVALUATION_REASON_PAYLOAD_LIMIT: &str = "payload_limit";

#[derive(Default)]
struct PayloadBuildResult {
    payloads: Vec<String>,
    dropped_oversized_rows: u64,
    degraded_oversized_rows: u64,
    payload_splits: u64,
}

#[derive(Default)]
pub(crate) struct FlagEvaluationTelemetryMetrics {
    pub(crate) rows_dropped_degraded_cap: u64,
    pub(crate) rows_dropped_payload_limit: u64,
    pub(crate) rows_degraded_cardinality_cap: u64,
    pub(crate) rows_degraded_payload_limit: u64,
    pub(crate) payload_splits: u64,
}

#[derive(Default)]
pub(crate) struct FlagEvaluationTelemetryCounters {
    rows_dropped_degraded_cap: AtomicU64,
    rows_dropped_payload_limit: AtomicU64,
    rows_degraded_cardinality_cap: AtomicU64,
    rows_degraded_payload_limit: AtomicU64,
    payload_splits: AtomicU64,
}

impl FlagEvaluationTelemetryCounters {
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

    pub(crate) fn collect_metrics(&self) -> FlagEvaluationTelemetryMetrics {
        FlagEvaluationTelemetryMetrics {
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
struct DestinationKey {
    url: String,
    timeout_ms: u64,
    test_token: Option<String>,
    use_system_resolver: bool,
    context: FfeTelemetryContext,
}

impl DestinationKey {
    fn new(endpoint: &Endpoint, context: &FfeTelemetryContext) -> Self {
        Self {
            url: endpoint.url.to_string(),
            timeout_ms: endpoint.timeout_ms,
            test_token: endpoint.test_token.as_ref().map(|s| s.to_string()),
            use_system_resolver: endpoint.use_system_resolver,
            context: context.clone(),
        }
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

struct PendingDestination {
    endpoint: Endpoint,
    context: FfeTelemetryContext,
    events: HashMap<EventKey, FfeFlagEvaluationEvent>,
}

#[derive(Default)]
struct CoalescerState {
    destinations: HashMap<DestinationKey, PendingDestination>,
    flush_running: bool,
    pending_bucket_count: usize,
    full_bucket_count: usize,
    full_bucket_count_by_flag: HashMap<String, usize>,
    degraded_bucket_count: usize,
    dropped_overflow: u64,
}

#[derive(Clone, Default)]
pub(crate) struct FlagEvaluationCoalescer {
    state: Arc<Mutex<CoalescerState>>,
    metrics: Arc<FlagEvaluationTelemetryCounters>,
}

impl FlagEvaluationCoalescer {
    pub(crate) fn enqueue(
        &self,
        client: NativeCapabilities,
        endpoint: Endpoint,
        batch: FfeFlagEvaluationBatch,
    ) {
        if batch.flag_evaluations.is_empty() {
            return;
        }

        let mut state = self.state.lock_or_panic();
        let destination_key = DestinationKey::new(&endpoint, &batch.context);
        state
            .destinations
            .entry(destination_key.clone())
            .or_insert_with(|| PendingDestination {
                endpoint,
                context: batch.context,
                events: HashMap::new(),
            });

        for mut event in batch.flag_evaluations {
            let key = EventKey::new(&event);
            if merge_pending_event(&mut state, &destination_key, &key, &event) {
                continue;
            }

            let flag_key = event.flag.key.clone();
            let full_bucket_count_for_flag = state
                .full_bucket_count_by_flag
                .get(&flag_key)
                .copied()
                .unwrap_or(0);

            if state.full_bucket_count < GLOBAL_CAP && full_bucket_count_for_flag < PER_FLAG_CAP {
                if insert_pending_event(&mut state, &destination_key, key, event) {
                    state.full_bucket_count += 1;
                    *state.full_bucket_count_by_flag.entry(flag_key).or_default() += 1;
                }
                continue;
            }

            event.targeting_key = None;
            event.context = None;
            let evaluation_count = event.evaluation_count;
            let degraded_key = EventKey::degraded(&event);
            if merge_pending_event(&mut state, &destination_key, &degraded_key, &event) {
                self.metrics
                    .add_rows_degraded_cardinality_cap(evaluation_count);
                continue;
            }

            if state.degraded_bucket_count >= DEGRADED_CAP
                || state.pending_bucket_count >= MAX_PENDING_BUCKETS
            {
                state.dropped_overflow = state.dropped_overflow.saturating_add(evaluation_count);
                self.metrics.add_rows_dropped_degraded_cap(evaluation_count);
                continue;
            }

            if insert_pending_event(&mut state, &destination_key, degraded_key, event) {
                state.degraded_bucket_count += 1;
                self.metrics
                    .add_rows_degraded_cardinality_cap(evaluation_count);
            }
        }

        if !state.flush_running {
            state.flush_running = true;
            let coalescer = self.clone();
            tokio::spawn(async move {
                coalescer.flush_loop(client).await;
            });
        }
    }

    pub(crate) async fn flush_now(&self, client: NativeCapabilities) {
        let batches = self.take_batches();
        futures::future::join_all(batches.into_iter().map(|(endpoint, batch)| {
            let client = client.clone();
            let metrics = Arc::clone(&self.metrics);
            async move { send_batch_with_metrics(&client, &endpoint, batch, &metrics).await }
        }))
        .await;
    }

    async fn flush_loop(self, client: NativeCapabilities) {
        loop {
            tokio::time::sleep(COALESCE_DELAY).await;
            let batches = self.take_batches();
            futures::future::join_all(batches.into_iter().map(|(endpoint, batch)| {
                let client = client.clone();
                let metrics = Arc::clone(&self.metrics);
                async move { send_batch_with_metrics(&client, &endpoint, batch, &metrics).await }
            }))
            .await;

            let mut state = self.state.lock_or_panic();
            if state.destinations.is_empty() {
                state.flush_running = false;
                break;
            }
        }
    }

    fn take_batches(&self) -> Vec<(Endpoint, FfeFlagEvaluationBatch)> {
        let mut state = self.state.lock_or_panic();
        if state.dropped_overflow > 0 {
            warn!(
                "ffe_flagevaluation_flusher: dropped {} pending bucket(s) after sidecar coalescer cap",
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
                    pending.endpoint,
                    FfeFlagEvaluationBatch {
                        context: pending.context,
                        flag_evaluations: pending.events.into_values().collect(),
                    },
                ))
            })
            .collect()
    }

    pub(crate) fn collect_metrics(&self) -> FlagEvaluationTelemetryMetrics {
        self.metrics.collect_metrics()
    }
}

fn merge_pending_event(
    state: &mut CoalescerState,
    destination_key: &DestinationKey,
    key: &EventKey,
    event: &FfeFlagEvaluationEvent,
) -> bool {
    let Some(pending) = state.destinations.get_mut(destination_key) else {
        return false;
    };

    if let Some(existing) = pending.events.get_mut(key) {
        merge_event(existing, event);
        true
    } else {
        false
    }
}

fn insert_pending_event(
    state: &mut CoalescerState,
    destination_key: &DestinationKey,
    key: EventKey,
    event: FfeFlagEvaluationEvent,
) -> bool {
    let Some(pending) = state.destinations.get_mut(destination_key) else {
        warn!("ffe_flagevaluation_flusher: missing pending destination; dropping event");
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

/// Build the FFE flagevaluation endpoint from a session's agent base endpoint.
/// Overrides only the path (`/evp_proxy/v2/api/v2/flagevaluation`), preserving
/// scheme, authority, timeout, and test_token.
/// Returns `None` for agentless mode because EVP proxy routing is agent-only.
pub(crate) fn flagevaluation_endpoint(base: &Endpoint) -> Option<Endpoint> {
    ffe_evp_proxy::endpoint(base, EVP_FLAGEVALUATION_PATH)
}

/// POST a structured FFE flag evaluation batch to the agent EVP proxy.
/// Fire-and-forget: non-2xx responses are logged at `warn`, network errors at
/// `debug`, and dropped (matches dd-trace-go behaviour).
#[cfg(test)]
async fn send_batch<C: HttpClientCapability + SleepCapability>(
    client: &C,
    endpoint: &Endpoint,
    batch: FfeFlagEvaluationBatch,
) {
    send_batch_with_limit(client, endpoint, batch, EVP_PAYLOAD_SIZE_LIMIT, None).await;
}

async fn send_batch_with_metrics<C: HttpClientCapability + SleepCapability>(
    client: &C,
    endpoint: &Endpoint,
    batch: FfeFlagEvaluationBatch,
    metrics: &FlagEvaluationTelemetryCounters,
) {
    send_batch_with_limit(
        client,
        endpoint,
        batch,
        EVP_PAYLOAD_SIZE_LIMIT,
        Some(metrics),
    )
    .await;
}

async fn send_batch_with_limit<C: HttpClientCapability + SleepCapability>(
    client: &C,
    endpoint: &Endpoint,
    batch: FfeFlagEvaluationBatch,
    payload_size_limit: usize,
    metrics: Option<&FlagEvaluationTelemetryCounters>,
) {
    let result = match build_payloads_for_post(batch, payload_size_limit) {
        Ok(result) => result,
        Err(e) => {
            debug!("ffe_flagevaluation_flusher: failed to encode batch payload: {e:?}");
            return;
        }
    };

    if let Some(metrics) = metrics {
        metrics.add_rows_dropped_payload_limit(result.dropped_oversized_rows);
        metrics.add_rows_degraded_payload_limit(result.degraded_oversized_rows);
        metrics.add_payload_splits(result.payload_splits);
    }

    if result.dropped_oversized_rows > 0 {
        warn!(
            "ffe_flagevaluation_flusher: dropped {} flag evaluation row(s) because they exceeded the {} byte EVP payload limit after degradation",
            result.dropped_oversized_rows,
            payload_size_limit
        );
    }

    for payload in result.payloads {
        ffe_evp_proxy::send_payload(
            client,
            endpoint,
            payload,
            LOG_PREFIX,
            "flag evaluation batch",
        )
        .await;
    }
}

fn build_payloads_for_post(
    batch: FfeFlagEvaluationBatch,
    payload_size_limit: usize,
) -> Result<PayloadBuildResult, serde_json::Error> {
    let FfeFlagEvaluationBatch {
        context,
        flag_evaluations,
    } = batch;

    let context_json = build_context_payload(&context)?;
    let payload_prefix = format!(r#"{{"context":{context_json},"flagEvaluations":["#);
    let payload_suffix = "]}";
    let base_payload_size = payload_prefix.len() + payload_suffix.len();

    let mut result = PayloadBuildResult::default();
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
            result.payload_splits = result.payload_splits.saturating_add(1);
        }

        let separator_size = usize::from(!current_events.is_empty());
        current_size += separator_size + event_size;
        current_events.push(encoded_event);
    }

    push_payload(
        &mut result.payloads,
        &payload_prefix,
        payload_suffix,
        &mut current_events,
    );
    if result.payloads.len() > 1 {
        result.payload_splits = result
            .payload_splits
            .max(result.payloads.len().saturating_sub(1) as u64);
    }

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

/// Build the EVP POST body from a batch.
///
/// The flagevaluation types are serialized over the sidecar's **bincode** IPC
/// wire, which is non-self-describing: a field omitted by `skip_serializing_if`
/// would misalign the derived `Deserialize` and cause the sidecar to drop the
/// whole batch. The types therefore carry **no** `skip_serializing_if` and emit
/// every field (optional ones as `null`/`false`/`""`). The flageval-worker EVP
/// schema, however, rejects those null/empty placeholders (especially for
/// degraded-tier events), so this helper strips them here, on the outbound POST
/// only — reproducing the old skip-serialization semantics without breaking the
/// bincode wire.
///
/// Two transforms happen, in order, on each `flagEvaluations` element:
///   1. `context.evaluation` is carried as a JSON-object **string** (bincode cannot encode
///      `serde_json::Value`); it is re-expanded back into a JSON **object** in place. An
///      unparseable string drops the field gracefully (never panics), matching the best-effort
///      telemetry contract.
///   2. The whole value is recursively cleaned (`strip_placeholders`) so the POST contains no
///      optional-field placeholders. `context.evaluation` user values are preserved as-is; boolean
///      `false`, empty strings, empty objects, and empty arrays are valid context values. Numeric
///      zeros (timestamps/counts) are preserved — they are real data.
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

    // Strip null/empty placeholders so the EVP POST matches the flageval-worker
    // schema (which rejects null placeholders) — see the function doc comment.
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
        // Re-expand the JSON-object string into an object in place.
        Ok(parsed) => *evaluation = parsed,
        // Unparseable string → drop the field gracefully (never panic).
        Err(_) => {
            if let Some(obj) = context.as_object_mut() {
                obj.remove("evaluation");
            }
        }
    }
}

/// Recursively remove placeholder entries from a JSON value so the EVP POST
/// carries no null/empty fields, reproducing the old `skip_serializing_if`
/// semantics on the outbound wire only.
///
/// An object entry is dropped when its value, after the children have
/// themselves been cleaned (bottom-up), is one of:
///   - `null`                         (was `Option::is_none`)
///   - `false` for `runtime_default_used`
///   - `""` for `service`
///   - `{}`              (an object that became empty after cleaning, e.g. a `context.dd` whose
///     only field `service` was stripped)
///   - `[]`              (an array that became empty after cleaning)
///
/// `context.evaluation` is not cleaned recursively because its children are
/// user context values, not optional-field placeholders.
///
/// Numeric values (including `0`) are NEVER removed — timestamps and counts are
/// real data. Non-zero bools (`true`) and non-empty strings/collections are
/// kept.
fn strip_placeholders(value: &mut serde_json::Value) {
    match value {
        serde_json::Value::Object(map) => {
            // Clean children first (bottom-up), then drop entries that are now
            // placeholders, so a container emptied by cleaning is itself removed.
            for (key, child) in map.iter_mut() {
                // `context.evaluation` contains user context values. Boolean
                // false, empty strings, empty objects, and empty arrays are
                // valid there and must not be stripped as optional-field
                // placeholders.
                if key != "evaluation" {
                    strip_placeholders(child);
                }
            }
            map.retain(|key, v| !is_placeholder(key, v));
        }
        serde_json::Value::Array(items) => {
            for item in items.iter_mut() {
                strip_placeholders(item);
            }
            items.retain(|v| !is_array_placeholder(v));
        }
        _ => {}
    }
}

/// Whether a (already-cleaned) JSON value is an empty/null placeholder that
/// should be dropped from the POST. Numeric zeros are NOT placeholders.
fn is_placeholder(key: &str, value: &serde_json::Value) -> bool {
    match value {
        serde_json::Value::Null => true,
        serde_json::Value::Bool(b) => key == "runtime_default_used" && !b,
        serde_json::Value::String(s) => key == "service" && s.is_empty(),
        serde_json::Value::Object(map) => map.is_empty(),
        serde_json::Value::Array(items) => items.is_empty(),
        // Numbers (incl. 0) are real data — never placeholders.
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::service::{FfeFlagEvaluationBatch, FfeTelemetryContext};
    use datadog_ffe::telemetry::flagevaluation::{
        AllocationKey, ContextDD, EvalError, FfeFlagEvaluationEvent, FlagEvalEventContext, FlagKey,
        TargetingRuleKey, VariantKey, PER_FLAG_CAP,
    };
    use httpmock::MockServer;
    use libdd_capabilities::{Bytes, HttpError, MaybeSend};
    use libdd_capabilities_impl::NativeCapabilities;
    use std::collections::BTreeMap;
    use std::future;

    fn endpoint_for(server: &MockServer) -> Endpoint {
        Endpoint {
            url: server.url("/").parse().unwrap(),
            ..Endpoint::default()
        }
    }

    fn context() -> FfeTelemetryContext {
        FfeTelemetryContext {
            service: "svc".to_owned(),
            env: "prod".to_owned(),
            version: "1".to_owned(),
        }
    }

    fn eval_event() -> FfeFlagEvaluationEvent {
        FfeFlagEvaluationEvent {
            timestamp: 1_700_000_000_000,
            flag: FlagKey {
                key: "my-flag".to_owned(),
            },
            first_evaluation: 1_699_999_990_000,
            last_evaluation: 1_700_000_000_000,
            evaluation_count: 5,
            variant: None,
            allocation: None,
            targeting_rule: None,
            targeting_key: None,
            // `evaluation` is carried as a JSON-object STRING on the wire (bincode
            // can't carry serde_json::Value); the flusher re-expands it to an object.
            context: Some(FlagEvalEventContext {
                evaluation: Some(
                    serde_json::to_string(&{
                        let mut m = BTreeMap::new();
                        m.insert("country".to_owned(), serde_json::json!("US"));
                        m
                    })
                    .unwrap(),
                ),
                dd: None,
            }),
            error: None,
            runtime_default_used: false,
        }
    }

    fn batch() -> FfeFlagEvaluationBatch {
        FfeFlagEvaluationBatch {
            context: context(),
            flag_evaluations: vec![eval_event()],
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
            targeting_rule: Some(TargetingRuleKey {
                key: "rule-1".to_owned(),
            }),
            targeting_key: Some("user-123".to_owned()),
            context: Some(FlagEvalEventContext {
                evaluation: Some(
                    serde_json::to_string(&{
                        let mut m = BTreeMap::new();
                        m.insert("plan".to_owned(), serde_json::json!("premium"));
                        m
                    })
                    .unwrap(),
                ),
                dd: Some(ContextDD {
                    service: "frontend".to_owned(),
                }),
            }),
            error: Some(EvalError {
                message: "boom".to_owned(),
            }),
            runtime_default_used: true,
        }
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

    #[test]
    fn build_payload_strips_degraded_tier_placeholders() {
        let batch = FfeFlagEvaluationBatch {
            context: context(),
            flag_evaluations: vec![degraded_event()],
        };
        let payload = build_payload(&batch).expect("build_payload must succeed");
        let v: serde_json::Value = serde_json::from_str(&payload).unwrap();
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
    fn build_payload_keeps_full_tier_fields() {
        let batch = FfeFlagEvaluationBatch {
            context: context(),
            flag_evaluations: vec![full_event()],
        };
        let payload = build_payload(&batch).expect("build_payload must succeed");
        let v: serde_json::Value = serde_json::from_str(&payload).unwrap();
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
    fn build_payload_collapses_empty_nested_context() {
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
        let v: serde_json::Value = serde_json::from_str(&payload).unwrap();

        assert!(
            v["flagEvaluations"][0].get("context").is_none(),
            "a context that becomes empty after cleaning must be removed entirely"
        );
    }

    #[test]
    fn build_payload_expands_evaluation_string_into_object() {
        let payload = build_payload(&batch()).expect("build_payload must succeed");
        let v: serde_json::Value = serde_json::from_str(&payload).unwrap();

        let evaluation = &v["flagEvaluations"][0]["context"]["evaluation"];
        assert!(
            evaluation.is_object(),
            "context.evaluation must be a JSON object in the POST body, not a string: {evaluation}"
        );
        assert_eq!(
            evaluation["country"], "US",
            "the expanded object must preserve the original key/value"
        );
        assert!(
            !evaluation.is_string(),
            "context.evaluation must not remain a quoted string"
        );
    }

    #[test]
    fn build_payload_drops_unparseable_evaluation_gracefully() {
        let mut batch = batch();
        batch.flag_evaluations[0].context = Some(FlagEvalEventContext {
            evaluation: Some("this is not json".to_owned()),
            dd: None,
        });

        let payload = build_payload(&batch).expect("build_payload must not fail on bad input");
        let v: serde_json::Value = serde_json::from_str(&payload).unwrap();

        assert!(
            v["flagEvaluations"][0]["context"]
                .get("evaluation")
                .is_none(),
            "unparseable evaluation must be dropped from the body"
        );
    }

    #[test]
    fn build_payload_preserves_false_and_empty_context_values() {
        let mut batch = batch();
        batch.flag_evaluations[0].context = Some(FlagEvalEventContext {
            evaluation: Some(
                serde_json::json!({
                    "enabled": false,
                    "empty": "",
                    "empty_object": {},
                    "empty_array": []
                })
                .to_string(),
            ),
            dd: None,
        });

        let payload = build_payload(&batch).expect("build_payload must succeed");
        let v: serde_json::Value = serde_json::from_str(&payload).unwrap();
        let evaluation = &v["flagEvaluations"][0]["context"]["evaluation"];

        assert_eq!(evaluation["enabled"], false);
        assert_eq!(evaluation["empty"], "");
        assert!(evaluation["empty_object"].is_object());
        assert!(evaluation["empty_array"].is_array());
    }

    #[test]
    fn build_payloads_for_post_splits_by_encoded_byte_limit() {
        let mut batch = batch();
        let event = batch.flag_evaluations[0].clone();
        batch.flag_evaluations = vec![event; 3];

        let one_event_limit = build_payload(&FfeFlagEvaluationBatch {
            context: context(),
            flag_evaluations: vec![batch.flag_evaluations[0].clone()],
        })
        .unwrap()
        .len();

        let result =
            build_payloads_for_post(batch, one_event_limit).expect("payload build must succeed");

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
    fn build_payloads_for_post_degrades_oversized_full_event_before_drop() {
        let mut oversized = full_event();
        oversized.context = Some(FlagEvalEventContext {
            evaluation: Some(
                serde_json::json!({
                    "blob": "x".repeat(1024),
                })
                .to_string(),
            ),
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

        let result = build_payloads_for_post(
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

        let v: serde_json::Value = serde_json::from_str(&result.payloads[0]).unwrap();
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
    fn build_payloads_for_post_drops_oversized_degraded_event() {
        let mut oversized = degraded_event();
        oversized.flag.key = "x".repeat(1024);

        let result = build_payloads_for_post(
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

    #[tokio::test]
    #[cfg_attr(miri, ignore)]
    async fn posts_to_evp_proxy() {
        let server = MockServer::start_async().await;
        let mock = server
            .mock_async(|when, then| {
                when.method(httpmock::Method::POST)
                    .path(EVP_FLAGEVALUATION_PATH)
                    .header(EVP_SUBDOMAIN_HEADER, EVP_SUBDOMAIN_VALUE)
                    .header("content-type", "application/json");
                then.status(202);
            })
            .await;

        let base = endpoint_for(&server);
        let ep = flagevaluation_endpoint(&base).unwrap();
        let client = NativeCapabilities::new_client();

        send_batch(&client, &ep, batch()).await;

        mock.assert_async().await;
        assert_eq!(mock.calls_async().await, 1);
    }

    #[tokio::test]
    #[cfg_attr(miri, ignore)]
    async fn splits_large_batches_before_posting() {
        let server = MockServer::start_async().await;
        let mock = server
            .mock_async(|when, then| {
                when.method(httpmock::Method::POST)
                    .path(EVP_FLAGEVALUATION_PATH)
                    .header(EVP_SUBDOMAIN_HEADER, EVP_SUBDOMAIN_VALUE)
                    .header("content-type", "application/json");
                then.status(202);
            })
            .await;

        let base = endpoint_for(&server);
        let ep = flagevaluation_endpoint(&base).unwrap();
        let client = NativeCapabilities::new_client();
        let mut batch = batch();
        let event = batch.flag_evaluations[0].clone();
        batch.flag_evaluations = vec![event; MAX_EVENTS_PER_POST * 2 + 1];

        send_batch(&client, &ep, batch).await;

        mock.assert_calls_async(3).await;
    }

    #[tokio::test]
    #[cfg_attr(miri, ignore)]
    async fn send_batch_splits_posts_by_encoded_byte_limit() {
        let server = MockServer::start_async().await;
        let mock = server
            .mock_async(|when, then| {
                when.method(httpmock::Method::POST)
                    .path(EVP_FLAGEVALUATION_PATH)
                    .header(EVP_SUBDOMAIN_HEADER, EVP_SUBDOMAIN_VALUE)
                    .header("content-type", "application/json");
                then.status(202);
            })
            .await;

        let base = endpoint_for(&server);
        let ep = flagevaluation_endpoint(&base).unwrap();
        let client = NativeCapabilities::new_client();
        let mut batch = batch();
        let event = batch.flag_evaluations[0].clone();
        batch.flag_evaluations = vec![event; 3];
        let one_event_limit = build_payload(&FfeFlagEvaluationBatch {
            context: context(),
            flag_evaluations: vec![batch.flag_evaluations[0].clone()],
        })
        .unwrap()
        .len();

        send_batch_with_limit(&client, &ep, batch, one_event_limit, None).await;

        mock.assert_calls_async(3).await;
    }

    #[tokio::test]
    #[cfg_attr(miri, ignore)]
    async fn coalesces_identical_batches_before_posting() {
        let server = MockServer::start_async().await;
        let mock = server
            .mock_async(|when, then| {
                when.method(httpmock::Method::POST)
                    .path(EVP_FLAGEVALUATION_PATH)
                    .body_includes("\"evaluation_count\":10");
                then.status(202);
            })
            .await;

        let base = endpoint_for(&server);
        let ep = flagevaluation_endpoint(&base).unwrap();
        let client = NativeCapabilities::new_client();
        let coalescer = FlagEvaluationCoalescer::default();

        coalescer.enqueue(client.clone(), ep.clone(), batch());
        coalescer.enqueue(client.clone(), ep, batch());

        for _ in 0..100 {
            if mock.calls_async().await == 1 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }

        mock.assert_calls_async(1).await;
    }

    #[test]
    fn coalescer_degrades_after_per_flag_cap() {
        let endpoint = Endpoint {
            url: "http://agent:8126".parse().unwrap(),
            ..Endpoint::default()
        };
        let ep = flagevaluation_endpoint(&endpoint).unwrap();
        let coalescer = FlagEvaluationCoalescer::default();
        coalescer.state.lock().unwrap().flush_running = true;

        let mut events = Vec::with_capacity(PER_FLAG_CAP + 50);
        for index in 0..(PER_FLAG_CAP + 50) {
            let mut event = full_event();
            event.evaluation_count = 1;
            event.targeting_key = Some(format!("user-{index}"));
            events.push(event);
        }

        coalescer.enqueue(
            NativeCapabilities::new_client(),
            ep,
            FfeFlagEvaluationBatch {
                context: context(),
                flag_evaluations: events,
            },
        );

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

        let metrics = coalescer.collect_metrics();
        assert_eq!(metrics.rows_degraded_cardinality_cap, 50);
        assert_eq!(metrics.rows_dropped_degraded_cap, 0);
    }

    #[test]
    fn coalescer_counts_degraded_cap_drops_by_evaluation_count() {
        let endpoint = Endpoint {
            url: "http://agent:8126".parse().unwrap(),
            ..Endpoint::default()
        };
        let ep = flagevaluation_endpoint(&endpoint).unwrap();
        let coalescer = FlagEvaluationCoalescer::default();
        {
            let mut state = coalescer.state.lock().unwrap();
            state.flush_running = true;
            state.full_bucket_count = GLOBAL_CAP;
            state.degraded_bucket_count = DEGRADED_CAP;
        }
        let mut event = full_event();
        event.evaluation_count = 9;

        coalescer.enqueue(
            NativeCapabilities::new_client(),
            ep,
            FfeFlagEvaluationBatch {
                context: context(),
                flag_evaluations: vec![event],
            },
        );

        let metrics = coalescer.collect_metrics();
        assert_eq!(metrics.rows_dropped_degraded_cap, 9);
        assert_eq!(metrics.rows_degraded_cardinality_cap, 0);
    }

    #[tokio::test]
    #[cfg_attr(miri, ignore)]
    async fn non_2xx_does_not_panic() {
        let server = MockServer::start_async().await;
        let _mock = server
            .mock_async(|when, then| {
                when.method(httpmock::Method::POST)
                    .path(EVP_FLAGEVALUATION_PATH);
                then.status(500).body("intake overloaded");
            })
            .await;

        let base = endpoint_for(&server);
        let ep = flagevaluation_endpoint(&base).unwrap();
        let client = NativeCapabilities::new_client();
        send_batch(&client, &ep, batch()).await;
        // Test passes if no panic occurs.
    }

    #[tokio::test]
    async fn timeout_returns_without_waiting_for_http_response() {
        let ep = Endpoint {
            url: "http://localhost:8126".parse().unwrap(),
            timeout_ms: 1,
            ..Endpoint::default()
        };

        send_batch(&HangingCapabilities, &ep, batch()).await;
        // Test passes if function returns before the pending HTTP future resolves.
    }

    #[test]
    fn endpoint_preserves_authority_overrides_path() {
        let base = Endpoint {
            url: "http://agent.internal:8126/v0.4/traces".parse().unwrap(),
            ..Endpoint::default()
        };
        let ep = flagevaluation_endpoint(&base).unwrap();
        assert_eq!(ep.url.scheme_str(), Some("http"));
        assert_eq!(ep.url.authority().unwrap().as_str(), "agent.internal:8126");
        assert_eq!(ep.url.path(), EVP_FLAGEVALUATION_PATH);
    }

    #[test]
    fn endpoint_rejects_agentless() {
        let base = Endpoint {
            url: "https://trace.agent.datadoghq.com/v0.4/traces"
                .parse()
                .unwrap(),
            api_key: Some("api-key".into()),
            ..Endpoint::default()
        };
        assert!(flagevaluation_endpoint(&base).is_none());
    }

    #[derive(Clone, Debug)]
    struct HangingCapabilities;

    impl HttpClientCapability for HangingCapabilities {
        fn new_client() -> Self {
            Self
        }

        fn request(
            &self,
            _req: http::Request<Bytes>,
        ) -> impl future::Future<Output = Result<http::Response<Bytes>, HttpError>> + MaybeSend
        {
            future::pending()
        }
    }

    impl SleepCapability for HangingCapabilities {
        fn new() -> Self {
            Self
        }

        async fn sleep(&self, _duration: Duration) {}
    }
}
