// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! Reusable FFE exposure payload and deduplication primitives.

use super::FfeTelemetryContext;
use super::features::features_map;
use lru::LruCache;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::num::NonZeroUsize;
use std::sync::{Arc, Mutex};

// Keep the default aligned with existing server SDK exposure caches: large
// enough for common per-process hot sets, but still bounded in sidecar memory.
const DEFAULT_CACHE_LIMIT: usize = 65_536;

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct FfeExposureBatch {
    pub context: FfeTelemetryContext,
    pub exposures: Vec<FfeExposure>,
}

#[derive(Clone, Deserialize, Eq, PartialEq, Serialize)]
pub struct FfeExposure {
    pub timestamp_ms: u64,
    pub flag_key: String,
    /// Empty subject ids are preserved intentionally. SDKs may evaluate with an
    /// empty targeting key, and a doLog=true exposure should still be emitted.
    pub subject_id: String,
    /// JSON object encoded by the tracer. Invalid or non-object JSON is treated
    /// as an empty object during EVP payload serialization.
    pub subject_attributes_json: String,
    pub allocation_key: String,
    pub variant: String,
    /// Serial id of the split the subject was assigned to. Zero-based per
    /// organization, so `Some(0)` is a real value and must not be treated as
    /// absent.
    pub serial_id: Option<i32>,
    /// Flat JSON object of the split features routed to exposures, canonical
    /// when it comes from the FFI. Empty, invalid or non-object JSON means no
    /// features; it is validated again when the payload is built. Not part of
    /// deduplication.
    pub features_json: String,
}

// Whole IPC requests are logged on receive and when an oversized send is rejected.
impl std::fmt::Debug for FfeExposure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FfeExposure")
            .field("timestamp_ms", &self.timestamp_ms)
            .field("serial_id", &self.serial_id)
            .finish_non_exhaustive()
    }
}

#[derive(Clone)]
pub struct ExposureDeduplicator {
    cache: Option<Arc<Mutex<LruCache<ExposureCacheKey, ExposureCacheValue>>>>,
}

impl Default for ExposureDeduplicator {
    fn default() -> Self {
        Self::new(DEFAULT_CACHE_LIMIT)
    }
}

impl ExposureDeduplicator {
    pub fn new(limit: usize) -> Self {
        Self {
            cache: NonZeroUsize::new(limit).map(|limit| Arc::new(Mutex::new(LruCache::new(limit)))),
        }
    }

    pub fn should_send(&self, context: &FfeTelemetryContext, exposure: &FfeExposure) -> bool {
        self.should_send_in_scope("", context, exposure)
    }

    /// Returns whether an exposure should be emitted within a producer-owned
    /// deduplication scope. The scope is bookkeeping only and is not serialized.
    pub fn should_send_in_scope(
        &self,
        scope: &str,
        context: &FfeTelemetryContext,
        exposure: &FfeExposure,
    ) -> bool {
        self.try_record_in_scope(scope, context, exposure, || {
            Ok::<_, std::convert::Infallible>(())
        })
        .unwrap_or_else(|never| match never {})
    }

    /// Publishes a changed assignment under the cache lock. A failed publication preserves the
    /// prior value.
    pub fn try_record_in_scope<E>(
        &self,
        scope: &str,
        context: &FfeTelemetryContext,
        exposure: &FfeExposure,
        publish: impl FnOnce() -> Result<(), E>,
    ) -> Result<bool, E> {
        let Some(cache) = &self.cache else {
            return publish().map(|()| true);
        };

        let (key, value) = assignment(scope, context, exposure);

        let mut cache = cache.lock().unwrap_or_else(|e| e.into_inner());
        if cache.get(&key).is_some_and(|cached| cached == &value) {
            return Ok(false);
        }

        publish()?;
        cache.put(key, value);
        Ok(true)
    }
    /// Publishes one ordered delivery chunk into capacity already reserved by the caller.
    /// Single admissions and chunk publications are serialized by the same assignment lock.
    pub fn publish_reserved_batch(
        &self,
        scope: &str,
        mut batch: FfeExposureBatch,
        publish: impl FnOnce(FfeExposureBatch),
    ) {
        let mut guard = self
            .cache
            .as_ref()
            .map(|c| c.lock().unwrap_or_else(|e| e.into_inner()));
        batch.exposures.retain(|event| {
            if !is_complete(event) {
                return false;
            }
            let Some(cache) = guard.as_mut() else {
                return true;
            };
            let (key, value) = assignment(scope, &batch.context, event);
            if cache.get(&key) == Some(&value) {
                return false;
            }
            cache.put(key, value);
            true
        });
        if !batch.exposures.is_empty() {
            publish(batch);
        }
    }
}

fn assignment(
    scope: &str,
    context: &FfeTelemetryContext,
    exposure: &FfeExposure,
) -> (ExposureCacheKey, ExposureCacheValue) {
    let key = ExposureCacheKey {
        scope: scope.to_owned(),
        service: context.service.clone(),
        env: context.env.clone(),
        version: context.version.clone(),
        flag_key: exposure.flag_key.clone(),
        subject_id: exposure.subject_id.clone(),
    };
    let value = ExposureCacheValue {
        allocation_key: exposure.allocation_key.clone(),
        variant: exposure.variant.clone(),
        serial_id: exposure.serial_id,
    };
    (key, value)
}

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
struct ExposureCacheKey {
    scope: String,
    service: String,
    env: String,
    version: String,
    flag_key: String,
    subject_id: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct ExposureCacheValue {
    allocation_key: String,
    variant: String,
    serial_id: Option<i32>,
}

pub fn encode_exposure_batch(
    deduplicator: &ExposureDeduplicator,
    batch: FfeExposureBatch,
) -> Result<Option<String>, serde_json::Error> {
    encode_exposure_batch_in_scope(deduplicator, "", batch)
}

/// Encodes an exposure batch while partitioning deduplication by destination
/// and logical producer identity.
pub fn encode_exposure_batch_in_scope(
    deduplicator: &ExposureDeduplicator,
    scope: &str,
    batch: FfeExposureBatch,
) -> Result<Option<String>, serde_json::Error> {
    let mut batch = batch;
    batch.exposures.retain(|event| {
        is_complete(event) && deduplicator.should_send_in_scope(scope, &batch.context, event)
    });
    encode_admitted_exposure_batch(batch)
}

/// Serializes exposures already admitted by the shared authority.
pub fn encode_admitted_exposure_batch(
    batch: FfeExposureBatch,
) -> Result<Option<String>, serde_json::Error> {
    let exposures = batch
        .exposures
        .into_iter()
        .map(ExposureEvent::from)
        .collect::<Vec<_>>();

    if exposures.is_empty() {
        return Ok(None);
    }

    let payload = ExposurePayload {
        context: ExposurePayloadContext::from(batch.context),
        exposures,
    };
    serde_json::to_string(&payload).map(Some)
}

/// Serializes ordered, previously admitted rows into independently valid HTTP bodies.
/// Callers validate that each individual row fits `limit` before admission.
pub fn admitted_exposure_payloads(
    batch: FfeExposureBatch,
    limit: usize,
) -> Result<impl Iterator<Item = Result<String, serde_json::Error>>, serde_json::Error> {
    use serde::ser::Error;
    let context = serde_json::to_string(&ExposurePayloadContext::from(batch.context))?;
    let prefix = format!(r#"{{"context":{context},"exposures":["#);
    let mut rows = batch
        .exposures
        .into_iter()
        .map(|row| serde_json::to_string(&ExposureEvent::from(row)));
    let mut pending = None;
    Ok(std::iter::from_fn(move || {
        let mut body = prefix.clone();
        while let Some(row) = pending.take().or_else(|| rows.next()) {
            let row = match row {
                Ok(row) => row,
                Err(e) => return Some(Err(e)),
            };
            let comma = body.len() != prefix.len();
            if body.len() + usize::from(comma) + row.len() + 2 > limit {
                if !comma {
                    return Some(Err(serde_json::Error::custom(
                        "exposure exceeds HTTP payload limit",
                    )));
                }
                pending = Some(Ok(row));
                break;
            }
            if comma {
                body.push(',');
            }
            body.push_str(&row);
        }
        if body.len() == prefix.len() {
            return None;
        }
        body.push_str("]}");
        Some(Ok(body))
    }))
}

pub fn is_complete(exposure: &FfeExposure) -> bool {
    // `subject_id` is intentionally not required here; empty targeting keys
    // are valid evaluation inputs and must not suppress exposure logging.
    !exposure.flag_key.is_empty()
        && !exposure.allocation_key.is_empty()
        && !exposure.variant.is_empty()
}

#[derive(Serialize)]
struct ExposurePayload {
    context: ExposurePayloadContext,
    exposures: Vec<ExposureEvent>,
}

#[derive(Serialize)]
struct ExposurePayloadContext {
    #[serde(skip_serializing_if = "String::is_empty")]
    service: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    env: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    version: String,
}

impl From<FfeTelemetryContext> for ExposurePayloadContext {
    fn from(value: FfeTelemetryContext) -> Self {
        Self {
            service: value.service,
            env: value.env,
            version: value.version,
        }
    }
}

#[derive(Serialize)]
struct ExposureEvent {
    timestamp: u64,
    allocation: Key,
    flag: Key,
    variant: Key,
    subject: Subject,
    #[serde(skip_serializing_if = "Option::is_none")]
    serial_id: Option<i32>,
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    features: BTreeMap<String, serde_json::Value>,
}

impl From<FfeExposure> for ExposureEvent {
    fn from(value: FfeExposure) -> Self {
        Self {
            timestamp: value.timestamp_ms,
            allocation: Key {
                key: value.allocation_key,
            },
            flag: Key {
                key: value.flag_key,
            },
            variant: Key { key: value.variant },
            subject: Subject {
                id: value.subject_id,
                attributes: subject_attributes(&value.subject_attributes_json),
            },
            serial_id: value.serial_id,
            features: features_map(&value.features_json),
        }
    }
}

#[derive(Serialize)]
struct Key {
    key: String,
}

#[derive(Serialize)]
struct Subject {
    id: String,
    attributes: serde_json::Map<String, serde_json::Value>,
}

fn subject_attributes(json: &str) -> serde_json::Map<String, serde_json::Value> {
    if json.is_empty() {
        return serde_json::Map::new();
    }

    match serde_json::from_str::<serde_json::Value>(json) {
        Ok(serde_json::Value::Object(attrs)) => attrs,
        Ok(_) => {
            log::debug!(
                "ffe exposure subject attributes must be a JSON object; using empty attributes"
            );
            serde_json::Map::new()
        }
        Err(error) => {
            log::debug!(
                "invalid ffe exposure subject attributes JSON: {error}; using empty attributes"
            );
            serde_json::Map::new()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::Value;

    fn context() -> FfeTelemetryContext {
        FfeTelemetryContext {
            service: "svc".to_owned(),
            env: "prod".to_owned(),
            version: "1".to_owned(),
        }
    }

    fn exposure(subject_id: &str, allocation_key: &str, variant: &str) -> FfeExposure {
        FfeExposure {
            timestamp_ms: 123,
            flag_key: "flag".to_owned(),
            subject_id: subject_id.to_owned(),
            subject_attributes_json: r#"{"tier":"premium"}"#.to_owned(),
            allocation_key: allocation_key.to_owned(),
            variant: variant.to_owned(),
            serial_id: None,
            features_json: String::new(),
        }
    }

    fn exposure_with_serial_id(
        subject_id: &str,
        allocation_key: &str,
        variant: &str,
        serial_id: i32,
    ) -> FfeExposure {
        FfeExposure {
            serial_id: Some(serial_id),
            ..exposure(subject_id, allocation_key, variant)
        }
    }

    #[test]
    fn http_batches_fit_the_encoded_byte_limit_and_preserve_all_rows() {
        let row = exposure("tab\tquote\"", "alloc", "on");
        let make = |n| FfeExposureBatch {
            context: context(),
            exposures: vec![row.clone(); n],
        };
        let exact = encode_admitted_exposure_batch(make(2)).unwrap().unwrap();
        let bodies: Vec<_> = admitted_exposure_payloads(make(5), exact.len())
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap();
        assert_eq!(
            bodies,
            [
                exact.clone(),
                exact.clone(),
                encode_admitted_exposure_batch(make(1)).unwrap().unwrap()
            ]
        );
        let one = encode_admitted_exposure_batch(make(1))
            .unwrap()
            .unwrap()
            .len();
        assert!(
            admitted_exposure_payloads(make(1), one - 1)
                .unwrap()
                .next()
                .unwrap()
                .is_err()
        );
    }

    #[test]
    fn exposure_batch_debug_redacts_identity_and_attributes() {
        let batch = FfeExposureBatch {
            context: context(),
            exposures: vec![exposure("private-identity-canary", "alloc", "variant")],
        };
        let debug = format!("{batch:?}");
        assert!(!debug.contains("private-identity-canary"));
        assert!(!debug.contains("premium"));
        assert!(debug.contains("timestamp_ms: 123"));
    }

    fn encode_one(deduplicator: &ExposureDeduplicator, exposure: FfeExposure) -> Option<Value> {
        encode_exposure_batch(
            deduplicator,
            FfeExposureBatch {
                context: context(),
                exposures: vec![exposure],
            },
        )
        .unwrap()
        .map(|payload| serde_json::from_str(&payload).unwrap())
    }

    #[test]
    fn failed_publication_preserves_the_last_admitted_assignment() {
        let cache = ExposureDeduplicator::new(1);
        let ctx = context();
        let a = exposure("user", "alloc", "a");
        let b = exposure("user", "alloc", "b");
        assert!(cache.should_send_in_scope("scope", &ctx, &a));
        assert_eq!(
            cache.try_record_in_scope("scope", &ctx, &b, || Err(())),
            Err(())
        );
        assert!(!cache.should_send_in_scope("scope", &ctx, &a));
        assert!(cache.should_send_in_scope("scope", &ctx, &b));
    }

    #[test]
    fn reserved_batch_commits_ordered_changes_to_the_delivery_cache() {
        let cache = ExposureDeduplicator::new(1);
        let a = exposure("user", "alloc", "a");
        let b = exposure("user", "alloc", "b");
        assert!(cache.should_send_in_scope("route", &context(), &a));
        let batch = FfeExposureBatch {
            context: context(),
            exposures: vec![a.clone(), b.clone(), b.clone(), a.clone()],
        };
        let mut sent = Vec::new();
        cache.publish_reserved_batch("route", batch, |chunk| sent = chunk.exposures);
        assert_eq!(sent, [b, a.clone()]);
        assert!(!cache.should_send_in_scope("route", &context(), &a));
        assert!(cache.should_send_in_scope("other", &context(), &a));
    }

    #[test]
    fn deduplication_is_partitioned_by_producer_scope() {
        let deduplicator = ExposureDeduplicator::new(4);
        let make_batch = || FfeExposureBatch {
            context: context(),
            exposures: vec![exposure("user", "alloc", "variant")],
        };

        assert!(
            encode_exposure_batch_in_scope(&deduplicator, "ruby/1.0", make_batch())
                .unwrap()
                .is_some()
        );
        assert!(
            encode_exposure_batch_in_scope(&deduplicator, "ruby/1.0", make_batch())
                .unwrap()
                .is_none()
        );
        assert!(
            encode_exposure_batch_in_scope(&deduplicator, "dotnet/1.0", make_batch())
                .unwrap()
                .is_some()
        );
    }

    #[test]
    fn encodes_structured_batch_and_preserves_empty_subject() {
        let deduplicator = ExposureDeduplicator::new(4);
        let payload = encode_exposure_batch(
            &deduplicator,
            FfeExposureBatch {
                context: context(),
                exposures: vec![exposure("", "alloc", "variant")],
            },
        )
        .unwrap()
        .unwrap();
        let payload: Value = serde_json::from_str(&payload).unwrap();

        assert_eq!(payload["context"]["service"], "svc");
        assert_eq!(payload["context"]["env"], "prod");
        assert_eq!(payload["context"]["version"], "1");
        assert_eq!(payload["exposures"][0]["subject"]["id"], "");
        assert_eq!(
            payload["exposures"][0]["subject"]["attributes"]["tier"],
            "premium"
        );
    }

    #[test]
    fn deduplicates_same_assignment_and_emits_changed_assignment() {
        let deduplicator = ExposureDeduplicator::new(4);
        let first = encode_exposure_batch(
            &deduplicator,
            FfeExposureBatch {
                context: context(),
                exposures: vec![exposure("user", "alloc-a", "a")],
            },
        )
        .unwrap();
        let duplicate = encode_exposure_batch(
            &deduplicator,
            FfeExposureBatch {
                context: context(),
                exposures: vec![exposure("user", "alloc-a", "a")],
            },
        )
        .unwrap();
        let changed = encode_exposure_batch(
            &deduplicator,
            FfeExposureBatch {
                context: context(),
                exposures: vec![exposure("user", "alloc-b", "b")],
            },
        )
        .unwrap();

        assert!(first.is_some());
        assert!(duplicate.is_none());
        assert!(changed.is_some());
    }

    #[test]
    fn zero_cache_limit_disables_deduplication() {
        let deduplicator = ExposureDeduplicator::new(0);
        let first = encode_exposure_batch(
            &deduplicator,
            FfeExposureBatch {
                context: context(),
                exposures: vec![exposure("user", "alloc", "variant")],
            },
        )
        .unwrap();
        let duplicate = encode_exposure_batch(
            &deduplicator,
            FfeExposureBatch {
                context: context(),
                exposures: vec![exposure("user", "alloc", "variant")],
            },
        )
        .unwrap();

        assert!(first.is_some());
        assert!(duplicate.is_some());
    }

    #[test]
    fn cache_key_includes_service_env_and_version() {
        let deduplicator = ExposureDeduplicator::new(4);
        let first = encode_exposure_batch(
            &deduplicator,
            FfeExposureBatch {
                context: context(),
                exposures: vec![exposure("user", "alloc", "variant")],
            },
        )
        .unwrap();
        let other_service = encode_exposure_batch(
            &deduplicator,
            FfeExposureBatch {
                context: FfeTelemetryContext {
                    service: "other".to_owned(),
                    ..context()
                },
                exposures: vec![exposure("user", "alloc", "variant")],
            },
        )
        .unwrap();

        assert!(first.is_some());
        assert!(other_service.is_some());
    }

    #[test]
    fn serializes_serial_id_at_the_event_top_level() {
        let payload = encode_one(
            &ExposureDeduplicator::new(4),
            exposure_with_serial_id("user", "alloc", "variant", 4242),
        )
        .unwrap();

        assert_eq!(payload["exposures"][0]["serial_id"], 4242);
    }

    #[test]
    fn serializes_a_zero_serial_id() {
        let payload = encode_one(
            &ExposureDeduplicator::new(4),
            exposure_with_serial_id("user", "alloc", "variant", 0),
        )
        .unwrap();

        assert_eq!(payload["exposures"][0]["serial_id"], 0);
    }

    #[test]
    fn omits_an_absent_serial_id() {
        let payload = encode_one(
            &ExposureDeduplicator::new(4),
            exposure("user", "alloc", "variant"),
        )
        .unwrap();

        assert_eq!(payload["exposures"][0].get("serial_id"), None);
    }

    #[test]
    fn emits_an_exposure_when_only_the_serial_id_appears() {
        let deduplicator = ExposureDeduplicator::new(4);
        let first = encode_one(&deduplicator, exposure("user", "alloc", "variant"));
        let appeared = encode_one(
            &deduplicator,
            exposure_with_serial_id("user", "alloc", "variant", 0),
        );

        assert_eq!(first.unwrap()["exposures"][0].get("serial_id"), None);
        assert_eq!(appeared.unwrap()["exposures"][0]["serial_id"], 0);
    }

    #[test]
    fn emits_an_exposure_when_only_the_serial_id_changes() {
        let deduplicator = ExposureDeduplicator::new(4);
        let first = encode_one(
            &deduplicator,
            exposure_with_serial_id("user", "alloc", "variant", 0),
        );
        let changed = encode_one(
            &deduplicator,
            exposure_with_serial_id("user", "alloc", "variant", 1),
        );

        assert_eq!(first.unwrap()["exposures"][0]["serial_id"], 0);
        assert_eq!(changed.unwrap()["exposures"][0]["serial_id"], 1);
    }

    #[test]
    fn emits_an_exposure_when_the_serial_id_disappears() {
        let deduplicator = ExposureDeduplicator::new(4);
        let first = encode_one(
            &deduplicator,
            exposure_with_serial_id("user", "alloc", "variant", 0),
        );
        let disappeared = encode_one(&deduplicator, exposure("user", "alloc", "variant"));

        assert_eq!(first.unwrap()["exposures"][0]["serial_id"], 0);
        assert_eq!(disappeared.unwrap()["exposures"][0].get("serial_id"), None);
    }

    #[test]
    fn emits_an_exposure_when_the_serial_id_returns_to_an_earlier_value() {
        let deduplicator = ExposureDeduplicator::new(4);
        let first = encode_one(
            &deduplicator,
            exposure_with_serial_id("user", "alloc", "variant", 7),
        );
        let changed = encode_one(
            &deduplicator,
            exposure_with_serial_id("user", "alloc", "variant", 9),
        );
        let returned = encode_one(
            &deduplicator,
            exposure_with_serial_id("user", "alloc", "variant", 7),
        );

        assert_eq!(first.unwrap()["exposures"][0]["serial_id"], 7);
        assert_eq!(changed.unwrap()["exposures"][0]["serial_id"], 9);
        assert_eq!(returned.unwrap()["exposures"][0]["serial_id"], 7);
    }

    #[test]
    fn deduplicates_an_unchanged_serial_id() {
        let deduplicator = ExposureDeduplicator::new(4);
        let first = encode_one(
            &deduplicator,
            exposure_with_serial_id("user", "alloc", "variant", 0),
        );
        let duplicate = encode_one(
            &deduplicator,
            exposure_with_serial_id("user", "alloc", "variant", 0),
        );

        assert_eq!(first.unwrap()["exposures"][0]["serial_id"], 0);
        assert_eq!(duplicate, None);
    }

    fn exposure_with_features(features_json: &str) -> FfeExposure {
        FfeExposure {
            features_json: features_json.to_owned(),
            ..exposure("user", "alloc", "variant")
        }
    }

    #[test]
    fn serializes_scalar_features_as_a_flat_object() {
        let payload = encode_one(
            &ExposureDeduplicator::new(4),
            exposure_with_features(
                r#"{"holdout.key":"h1","weight":2,"included":false,"nested":{"a":1}}"#,
            ),
        )
        .unwrap();

        assert_eq!(
            payload["exposures"][0]["features"],
            serde_json::json!({"holdout.key":"h1","weight":2.0,"included":false})
        );
    }

    #[test]
    fn omits_features_when_none_survive_and_keeps_the_exposure() {
        for features_json in ["", "not json", "[1]", "{}", r#"{"a":null}"#] {
            let payload = encode_one(
                &ExposureDeduplicator::new(4),
                exposure_with_features(features_json),
            )
            .unwrap();

            assert_eq!(payload["exposures"][0].get("features"), None);
            assert_eq!(payload["exposures"][0]["flag"]["key"], "flag");
        }
    }

    #[test]
    fn exposure_features_are_truncated_at_the_byte_cap() {
        use crate::telemetry::features::{MAX_FEATURES_BYTES, features_of_encoded_len};
        let over_cap = features_of_encoded_len(MAX_FEATURES_BYTES + 1);
        let payload = encode_one(
            &ExposureDeduplicator::new(4),
            exposure_with_features(&over_cap),
        )
        .unwrap();
        let features = &payload["exposures"][0]["features"];
        let input: serde_json::Map<String, Value> = serde_json::from_str(&over_cap).unwrap();
        assert_eq!(features.as_object().unwrap().len(), input.len() - 1);
        assert!(serde_json::to_string(features).unwrap().len() <= MAX_FEATURES_BYTES);
    }

    #[test]
    fn features_round_trip_via_bincode() {
        let batch = FfeExposureBatch {
            context: context(),
            exposures: vec![
                exposure_with_features(r#"{"a":1}"#),
                exposure("user", "alloc", "variant"),
            ],
        };
        let bytes = bincode::serialize(&batch).unwrap();
        let decoded: FfeExposureBatch = bincode::deserialize(&bytes).unwrap();
        assert_eq!(decoded, batch);
    }

    #[test]
    fn features_do_not_affect_deduplication() {
        let deduplicator = ExposureDeduplicator::new(4);
        let first = encode_one(&deduplicator, exposure_with_features(r#"{"a":"1"}"#));
        let changed = encode_one(&deduplicator, exposure_with_features(r#"{"a":"2"}"#));

        assert_eq!(first.unwrap()["exposures"][0]["features"]["a"], "1");
        assert_eq!(changed, None);
    }

    #[test]
    fn drops_incomplete_exposures() {
        let deduplicator = ExposureDeduplicator::new(4);
        let mut invalid = exposure("user", "alloc", "variant");
        invalid.allocation_key.clear();

        assert!(
            encode_exposure_batch(
                &deduplicator,
                FfeExposureBatch {
                    context: context(),
                    exposures: vec![invalid],
                },
            )
            .unwrap()
            .is_none()
        );
    }
}
