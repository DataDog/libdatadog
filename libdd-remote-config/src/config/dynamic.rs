// Copyright 2025-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use serde::{Deserialize, Serialize};
use std::collections::HashMap;

#[derive(Debug, Clone, Deserialize)]
#[cfg_attr(feature = "test", derive(Default, Serialize))]
pub struct DynamicConfigTarget {
    #[serde(default)]
    pub service: Option<String>,
    #[serde(default)]
    pub env: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[cfg_attr(feature = "test", derive(Serialize))]
pub struct DynamicConfigFile {
    pub action: String,
    #[serde(default)]
    pub service_target: Option<DynamicConfigTarget>,
    pub lib_config: DynamicConfig,
}

impl DynamicConfigFile {
    /// Returns the priority of this config for merge ordering.
    /// Lower value = higher priority.
    /// 0 = service+env specific, 1 = service only, 2 = env only,
    /// 3 = reserved (k8s cluster), 4 = org-level (wildcard/absent)
    pub fn priority(&self) -> u8 {
        fn is_specific(s: &Option<String>) -> bool {
            s.as_deref().is_some_and(|v| v != "*")
        }
        match &self.service_target {
            None => 4,
            Some(t) => match (is_specific(&t.service), is_specific(&t.env)) {
                (true, true) => 0,
                (true, false) => 1,
                (false, true) => 2,
                (false, false) => 4,
            },
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[cfg_attr(feature = "test", derive(Serialize))]
pub(crate) struct TracingHeaderTag {
    pub header: String,
    pub tag_name: String,
}

#[derive(Debug, Copy, Clone, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TracingSamplingRuleProvenance {
    Customer,
    Dynamic,
}

#[derive(Debug, Clone, Deserialize)]
#[cfg_attr(feature = "test", derive(Serialize))]
pub struct TracingSamplingRuleTag {
    pub key: String,
    pub value_glob: String,
}

#[derive(Debug, Clone, Deserialize)]
#[cfg_attr(feature = "test", derive(Serialize))]
pub struct TracingSamplingRule {
    pub service: String,
    pub name: Option<String>,
    pub provenance: TracingSamplingRuleProvenance,
    pub resource: String,
    #[serde(default)]
    pub tags: Vec<TracingSamplingRuleTag>,
    pub sample_rate: f64,
}

#[derive(Debug, Clone, Deserialize)]
#[cfg_attr(feature = "test", derive(Default, Serialize))]
pub struct DynamicConfig {
    pub(crate) tracing_header_tags: Option<Vec<TracingHeaderTag>>,
    pub(crate) tracing_sampling_rate: Option<f64>,
    pub(crate) log_injection_enabled: Option<bool>,
    pub(crate) tracing_tags: Option<Vec<String>>,
    pub(crate) tracing_enabled: Option<bool>,
    pub(crate) tracing_sampling_rules: Option<Vec<TracingSamplingRule>>,
    pub(crate) dynamic_instrumentation_enabled: Option<bool>,
    pub(crate) exception_replay_enabled: Option<bool>,
    pub(crate) code_origin_enabled: Option<bool>,
}

impl From<DynamicConfig> for Vec<Configs> {
    fn from(value: DynamicConfig) -> Self {
        let mut vec = Vec::with_capacity(9);
        if let Some(tags) = value.tracing_header_tags {
            vec.push(Configs::TracingHeaderTags(
                tags.into_iter().map(|t| (t.header, t.tag_name)).collect(),
            ));
        }
        if let Some(sampling_rate) = value.tracing_sampling_rate {
            vec.push(Configs::TracingSamplingRate(sampling_rate));
        }
        if let Some(log_injection) = value.log_injection_enabled {
            vec.push(Configs::LogInjectionEnabled(log_injection));
        }
        if let Some(tags) = value.tracing_tags {
            vec.push(Configs::TracingTags(tags));
        }
        if let Some(enabled) = value.tracing_enabled {
            vec.push(Configs::TracingEnabled(enabled));
        }
        if let Some(sampling_rules) = value.tracing_sampling_rules {
            vec.push(Configs::TracingSamplingRules(sampling_rules));
        }
        if let Some(enabled) = value.dynamic_instrumentation_enabled {
            vec.push(Configs::DynamicInstrumentationEnabled(enabled));
        }
        if let Some(enabled) = value.exception_replay_enabled {
            vec.push(Configs::ExceptionReplayEnabled(enabled));
        }
        if let Some(enabled) = value.code_origin_enabled {
            vec.push(Configs::CodeOriginEnabled(enabled));
        }
        vec
    }
}

#[derive(Clone)]
pub enum Configs {
    TracingHeaderTags(HashMap<String, String>),
    TracingSamplingRate(f64),
    LogInjectionEnabled(bool),
    TracingTags(Vec<String>), // "key:val" format
    TracingEnabled(bool),
    TracingSamplingRules(Vec<TracingSamplingRule>),
    DynamicInstrumentationEnabled(bool),
    ExceptionReplayEnabled(bool),
    CodeOriginEnabled(bool),
}

pub fn parse_json(data: &[u8]) -> serde_json::error::Result<DynamicConfigFile> {
    serde_json::from_slice(data)
}

#[cfg(feature = "test")]
pub mod tests {
    use super::*;

    pub fn dummy_dynamic_config(enabled: bool) -> DynamicConfigFile {
        DynamicConfigFile {
            action: "".to_string(),
            service_target: None,
            lib_config: DynamicConfig {
                tracing_enabled: Some(enabled),
                ..DynamicConfig::default()
            },
        }
    }

    #[test]
    fn absent_field_emits_no_variant() {
        let cfg: DynamicConfigFile = parse_json(br#"{"action": "", "lib_config": {}}"#).unwrap();
        assert!(cfg.lib_config.tracing_sampling_rate.is_none());
        assert!(<Vec<Configs>>::from(cfg.lib_config).is_empty());
    }

    #[test]
    fn explicit_null_is_indistinguishable_from_absent() {
        // No three-state model: null and absent both become `None`, so
        // neither produces a `Configs` variant. Clearing prior remote state
        // is the file-level responsibility (file removal), not an in-file
        // signal.
        let cfg: DynamicConfigFile =
            parse_json(br#"{"action": "", "lib_config": {"tracing_sampling_rate": null}}"#)
                .unwrap();
        assert!(cfg.lib_config.tracing_sampling_rate.is_none());
        assert!(<Vec<Configs>>::from(cfg.lib_config).is_empty());
    }

    #[test]
    fn concrete_value_emits_set_variant() {
        let cfg: DynamicConfigFile =
            parse_json(br#"{"action": "", "lib_config": {"tracing_sampling_rate": 0.25}}"#)
                .unwrap();
        assert_eq!(cfg.lib_config.tracing_sampling_rate, Some(0.25));
        let configs: Vec<Configs> = cfg.lib_config.into();
        assert_eq!(configs.len(), 1);
        assert!(matches!(configs[0], Configs::TracingSamplingRate(r) if r == 0.25));
    }

    #[test]
    fn unrelated_field_does_not_emit_sampling_variants() {
        // Regression guard: a payload that updates only `tracing_tags` must
        // not produce a phantom `TracingSamplingRate` / `TracingSamplingRules`
        // variant. Each field's absence is independent.
        let cfg: DynamicConfigFile =
            parse_json(br#"{"action": "", "lib_config": {"tracing_tags": ["foo:bar"]}}"#).unwrap();
        let configs: Vec<Configs> = cfg.lib_config.into();
        assert_eq!(configs.len(), 1);
        assert!(matches!(configs[0], Configs::TracingTags(_)));
        assert!(!configs
            .iter()
            .any(|c| matches!(c, Configs::TracingSamplingRate(_))));
        assert!(!configs
            .iter()
            .any(|c| matches!(c, Configs::TracingSamplingRules(_))));
    }
}
