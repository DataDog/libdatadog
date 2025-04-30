// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use serde::{Deserialize, Serialize};

#[derive(Debug, Deserialize)]
#[cfg_attr(feature = "test", derive(Default, Serialize))]
pub struct DynamicConfigTarget {
    pub service: String,
    pub env: String,
}

#[derive(Debug, Deserialize)]
#[cfg_attr(feature = "test", derive(Serialize))]
pub struct DynamicConfigFile {
    pub action: String,
    pub service_target: DynamicConfigTarget,
    pub lib_config: DynamicConfig,
}

#[derive(Debug, Deserialize)]
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

#[derive(Debug, Deserialize)]
#[cfg_attr(feature = "test", derive(Serialize))]
pub struct TracingSamplingRuleTag {
    pub key: String,
    pub value_glob: String,
}

#[derive(Debug, Deserialize)]
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

#[derive(Debug, Deserialize)]
#[cfg_attr(feature = "test", derive(Default, Serialize))]
pub struct DynamicConfig {
    pub(crate) tracing_header_tags: Option<Vec<TracingHeaderTag>>,
    pub(crate) tracing_sample_rate: Option<f64>,
    pub(crate) log_injection_enabled: Option<bool>,
    pub(crate) tracing_tags: Option<Vec<String>>,
    pub(crate) tracing_enabled: Option<bool>,
    pub(crate) tracing_sampling_rules: Option<Vec<TracingSamplingRule>>,
}

#[cfg(feature = "test")]
pub mod tests {
    use super::*;

    pub fn dummy_dynamic_config(enabled: bool) -> DynamicConfigFile {
        DynamicConfigFile {
            action: "".to_string(),
            service_target: DynamicConfigTarget::default(),
            lib_config: DynamicConfig {
                tracing_enabled: Some(enabled),
                ..DynamicConfig::default()
            },
        }
    }
}
