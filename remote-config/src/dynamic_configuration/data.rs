use std::collections::HashMap;
use serde::{Deserialize, Serialize};

#[derive(Debug, Deserialize)]
pub struct DynamicConfigTarget {
    pub service: String,
    pub env: String,
}

#[derive(Debug, Deserialize)]
pub struct DynamicConfigFile {
    pub action: String,
    pub service_target: DynamicConfigTarget,
    pub lib_config: DynamicConfig,
}

#[derive(Debug, Deserialize)]
struct TracingHeaderTag {
    header: String,
    tag_name: String,
}

#[derive(Debug, Copy, Clone, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TracingSamplingRuleProvenance {
    Customer,
    Dynamic,
}

#[derive(Debug, Deserialize)]
pub struct TracingSamplingRuleTag {
    pub key: String,
    pub value_glob: String,
}

#[derive(Debug, Deserialize)]
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
pub struct DynamicConfig {
    tracing_header_tags: Option<Vec<TracingHeaderTag>>,
    tracing_sample_rate: Option<f64>,
    log_injection_enabled: Option<bool>,
    tracing_tags: Option<Vec<String>>,
    tracing_enabled: Option<bool>,
    tracing_sampling_rules: Option<Vec<TracingSamplingRule>>,
}

impl From<DynamicConfig> for Vec<Configs> {
    fn from(value: DynamicConfig) -> Self {
        let mut vec = vec![];
        if let Some(tags) = value.tracing_header_tags {
            vec.push(Configs::TracingHeaderTags(tags.into_iter().map(|t| (t.header, t.tag_name)).collect()))
        }
        if let Some(sample_rate) = value.tracing_sample_rate {
            vec.push(Configs::TracingSampleRate(sample_rate));
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
        vec
    }
}

pub enum Configs {
    TracingHeaderTags(HashMap<String, String>),
    TracingSampleRate(f64),
    LogInjectionEnabled(bool),
    TracingTags(Vec<String>), // "key:val" format
    TracingEnabled(bool),
    TracingSamplingRules(Vec<TracingSamplingRule>),
}
