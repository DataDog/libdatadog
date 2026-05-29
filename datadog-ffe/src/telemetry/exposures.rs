// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! Reusable FFE exposure payload and deduplication primitives.

use super::FfeTelemetryContext;
use lru::LruCache;
use serde::{Deserialize, Serialize};
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

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct FfeExposure {
    pub timestamp_ms: u64,
    pub flag_key: String,
    pub subject_id: String,
    /// JSON object encoded by the tracer. Invalid or non-object JSON is treated
    /// as an empty object during EVP payload serialization.
    pub subject_attributes_json: String,
    pub allocation_key: String,
    pub variant: String,
}

#[derive(Clone)]
pub struct ExposureDeduplicator {
    cache: Arc<Mutex<LruCache<ExposureCacheKey, ExposureCacheValue>>>,
}

impl Default for ExposureDeduplicator {
    fn default() -> Self {
        Self::new(DEFAULT_CACHE_LIMIT)
    }
}

impl ExposureDeduplicator {
    pub fn new(limit: usize) -> Self {
        let limit = NonZeroUsize::new(limit).unwrap_or(NonZeroUsize::MIN);
        Self {
            cache: Arc::new(Mutex::new(LruCache::new(limit))),
        }
    }

    pub fn should_send(&self, context: &FfeTelemetryContext, exposure: &FfeExposure) -> bool {
        let key = ExposureCacheKey {
            service: context.service.clone(),
            env: context.env.clone(),
            version: context.version.clone(),
            flag_key: exposure.flag_key.clone(),
            subject_id: exposure.subject_id.clone(),
        };
        let value = ExposureCacheValue {
            allocation_key: exposure.allocation_key.clone(),
            variant: exposure.variant.clone(),
        };

        let mut cache = self.cache.lock().unwrap_or_else(|e| e.into_inner());
        if cache.get(&key).is_some_and(|cached| cached == &value) {
            return false;
        }

        cache.put(key, value);
        true
    }
}

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
struct ExposureCacheKey {
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
}

pub fn encode_exposure_batch(
    deduplicator: &ExposureDeduplicator,
    batch: FfeExposureBatch,
) -> Result<Option<String>, serde_json::Error> {
    let exposures = batch
        .exposures
        .into_iter()
        .filter(is_complete)
        .filter(|exposure| deduplicator.should_send(&batch.context, exposure))
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

fn is_complete(exposure: &FfeExposure) -> bool {
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
        }
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
    fn drops_incomplete_exposures() {
        let deduplicator = ExposureDeduplicator::new(4);
        let mut invalid = exposure("user", "alloc", "variant");
        invalid.allocation_key.clear();

        assert!(encode_exposure_batch(
            &deduplicator,
            FfeExposureBatch {
                context: context(),
                exposures: vec![invalid],
            },
        )
        .unwrap()
        .is_none());
    }
}
