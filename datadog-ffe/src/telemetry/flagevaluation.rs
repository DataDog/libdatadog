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
//!
//! Serialization note (bincode wire vs EVP POST): these types cross the
//! worker→sidecar IPC boundary, which is encoded with **bincode** — a
//! non-self-describing format whose derived `Deserialize` reads every field in
//! declaration order. `#[serde(skip_serializing_if = ...)]` is therefore
//! **incompatible** with the bincode wire: a skipped field is omitted on
//! serialize but still expected on deserialize, causing field misalignment and
//! a dropped batch. For that reason **all fields here are always serialized**
//! (no `skip_serializing_if`). The flageval-worker EVP schema rejects null /
//! empty placeholders (especially for degraded-tier events), so the sidecar
//! flusher (`ffe_flagevaluation_flusher::build_payload`) strips null / empty
//! placeholder entries from the JSON before the HTTP POST, reproducing the old
//! skip semantics only on the outbound wire. `#[serde(default)]` is kept on
//! fields that have it for deserialize robustness.

use super::FfeTelemetryContext;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

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

// ── Context pruning bounds ───────────────────────────────────────────────────
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
/// **All fields are always serialized** (no `skip_serializing_if`) so the type
/// is safe over the non-self-describing bincode IPC wire (see the module-level
/// serialization note). The degraded tier therefore serializes optional fields
/// as `null`/`false` on the wire; the sidecar flusher
/// (`ffe_flagevaluation_flusher::build_payload`) strips those null/empty
/// placeholders before the EVP POST so the flageval-worker schema sees no null
/// placeholders.
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
    /// sidecar flusher (`ffe_flagevaluation_flusher::build_payload`) before the
    /// EVP POST, so the on-the-wire EVP schema (`context.evaluation` as an
    /// object) is unchanged. `Eq` is preserved because `String` is `Eq`.
    ///
    /// Always serialized (no `skip_serializing_if`) for bincode-wire safety;
    /// the sidecar flusher strips it when `None` → `null`.
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
    // happens in the sidecar flusher's `build_payload`, not at the type level.

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
}
