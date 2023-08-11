use std::collections::HashMap;
use serde::Deserialize;

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

#[derive(Debug, Deserialize)]
pub struct DynamicConfig {
    tracing_header_tags: Option<Vec<TracingHeaderTag>>,
    tracing_sample_rate: Option<f64>,
    log_injection_enabled: Option<bool>,
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
        vec
    }
}

pub enum Configs {
    TracingHeaderTags(HashMap<String, String>),
    TracingSampleRate(f64),
    LogInjectionEnabled(bool),
}
