// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! EVP flagevaluation payload and serialization primitives for the
//! `flageval-worker` ingestion schema.
//!
//! Crate-naming note: this workspace uses `libdd-remote-config` (not
//! `datadog-remote-config`) for the remote config crate. Downstream consumers
//! (e.g. `dd-trace-php`) must use `libdd-remote-config` in any import paths.
//!
//! Two-tier aggregation (full → degraded → drop-counted) and context pruning
//! are enforced by the caller (PHP sidecar bridge, 02-07). This module only
//! owns the payload types and serialization helpers.

use super::FfeTelemetryContext;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

// ── Aggregation caps (frozen contract §1) ────────────────────────────────────
/// Maximum number of distinct full-tier buckets across all flags.
pub const GLOBAL_CAP: usize = 131_072;
/// Maximum number of full-tier buckets for a single flag.
pub const PER_FLAG_CAP: usize = 10_000;
/// Maximum number of distinct degraded-tier buckets across all flags.
pub const DEGRADED_CAP: usize = 32_768;

// ── Context pruning bounds (reviewer concern #1 review:4477935835) ────────────
/// Maximum number of context fields to include in a full-tier event.
pub const MAX_CONTEXT_FIELDS: usize = 256;
/// Maximum byte length of a context field value string. Values exceeding this
/// are skipped entirely (not truncated) to avoid partial-data misattribution.
pub const MAX_FIELD_LENGTH: usize = 256;

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
/// Required fields are always present. Optional fields use
/// `skip_serializing_if = "Option::is_none"` (or the bool equivalent) so the
/// degraded tier emits a valid schema object without any null placeholders
/// (reviewer concern #2 review:4477935835).
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

    // Optional fields — present in the full tier, absent in the degraded tier.

    /// Variant key; absent when the evaluation returned the runtime default
    /// (no variant assigned).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub variant: Option<VariantKey>,
    /// Allocation key from the UFC rule that produced this evaluation.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub allocation: Option<AllocationKey>,
    /// Targeting key identifying the evaluation subject.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub targeting_key: Option<String>,
    /// Pruned evaluation context (≤256 fields, values ≤256 chars, skip-not-truncate).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub context: Option<FlagEvalEventContext>,
    /// Evaluation error, if any.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<EvalError>,

    // Optional fields — may appear in either tier.

    /// `true` when the evaluation returned the SDK runtime default (absent
    /// variant, not a UFC-assigned variant). Omitted when false; defaults to
    /// `false` on deserialization when absent.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
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
    /// Pruned evaluation context attributes (≤256 fields, values ≤256 chars).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub evaluation: Option<BTreeMap<String, serde_json::Value>>,
    /// Datadog-specific context sub-object.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub dd: Option<ContextDD>,
}

/// Datadog-specific context fields inside the per-event `context.dd` object.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ContextDD {
    #[serde(skip_serializing_if = "String::is_empty")]
    pub service: String,
}

// ── Context pruning ──────────────────────────────────────────────────────────

/// Prune evaluation context attributes to satisfy the frozen contract bounds:
/// - At most `MAX_CONTEXT_FIELDS` (256) entries are kept.
/// - String values longer than `MAX_FIELD_LENGTH` (256 chars) are **skipped**
///   (not truncated) to avoid partial-data misattribution.
/// - Non-string values (bool, number, null) are kept regardless of
///   their display length.
/// - Keys are iterated in sorted order for deterministic canonical-key
///   stability; the returned `BTreeMap` preserves that order.
///
/// This satisfies reviewer concern #1 (`review:4477935835`).
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
            context: Some(FlagEvalEventContext {
                evaluation: Some({
                    let mut m = BTreeMap::new();
                    m.insert("plan".to_owned(), json!("premium"));
                    m
                }),
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

    // ── Test: degraded-tier event omits optional fields (no null) ─────────────

    #[test]
    fn degraded_tier_event_omits_optional_fields_not_null() {
        let degraded = FfeFlagEvaluationEvent {
            timestamp: 1_700_000_000_000,
            flag: FlagKey {
                key: "flag-b".to_owned(),
            },
            first_evaluation: 1_699_999_990_000,
            last_evaluation: 1_700_000_000_000,
            evaluation_count: 7,
            variant: None,
            allocation: None,
            targeting_key: None,
            context: None,
            error: None,
            runtime_default_used: false,
        };
        let json = serde_json::to_string(&degraded).unwrap();
        let v: Value = serde_json::from_str(&json).unwrap();

        // Required fields present.
        assert_eq!(v["flag"]["key"], "flag-b");
        assert!(v["first_evaluation"].is_number());
        assert!(v["last_evaluation"].is_number());
        assert_eq!(v["evaluation_count"], 7);

        // Optional fields entirely absent (not null).
        assert!(v.get("variant").is_none(), "variant should be absent");
        assert!(v.get("allocation").is_none(), "allocation should be absent");
        assert!(
            v.get("targeting_key").is_none(),
            "targeting_key should be absent"
        );
        assert!(v.get("context").is_none(), "context should be absent");
        assert!(v.get("error").is_none(), "error should be absent");
        assert!(
            v.get("runtime_default_used").is_none(),
            "runtime_default_used should be absent when false"
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
}
