// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use crate::data::{DynamicConfig, DynamicConfigFile, TracingSamplingRule};
use std::collections::HashMap;

pub mod data;

impl From<DynamicConfig> for Vec<Configs> {
    fn from(value: DynamicConfig) -> Self {
        let mut vec = vec![];
        if let Some(tags) = value.tracing_header_tags {
            vec.push(Configs::TracingHeaderTags(
                tags.into_iter().map(|t| (t.header, t.tag_name)).collect(),
            ))
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

pub fn parse_json(data: &[u8]) -> serde_json::error::Result<DynamicConfigFile> {
    serde_json::from_slice(data)
}
