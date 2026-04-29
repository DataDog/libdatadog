// Copyright 2025-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fmt::Display;
use std::ops::Deref;
use std::str::FromStr;

/// Configuration for a single sampling rule
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct SamplingRuleConfig {
    /// The sample rate to apply (0.0-1.0)
    pub sample_rate: f64,

    /// Optional service name pattern to match
    #[serde(default)]
    pub service: Option<String>,

    /// Optional span name pattern to match
    #[serde(default)]
    pub name: Option<String>,

    /// Optional resource name pattern to match
    #[serde(default)]
    pub resource: Option<String>,

    /// Tags that must match (key-value pairs)
    #[serde(default)]
    pub tags: HashMap<String, String>,

    /// Where this rule comes from (customer, dynamic, default).
    /// Not exposed in the public `datadog-opentelemetry` API — set automatically
    /// during conversion from the public `SamplingRuleConfig` type.
    #[serde(default = "default_provenance")]
    pub provenance: String,
}

impl Display for SamplingRuleConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", serde_json::json!(self))
    }
}

fn default_provenance() -> String {
    "default".to_string()
}

#[derive(Debug, Default, Clone, PartialEq)]
pub struct ParsedSamplingRules {
    pub rules: Vec<SamplingRuleConfig>,
}

impl Deref for ParsedSamplingRules {
    type Target = [SamplingRuleConfig];

    fn deref(&self) -> &Self::Target {
        &self.rules
    }
}

impl From<ParsedSamplingRules> for Vec<SamplingRuleConfig> {
    fn from(parsed: ParsedSamplingRules) -> Self {
        parsed.rules
    }
}

impl FromStr for ParsedSamplingRules {
    type Err = serde_json::Error;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        if s.trim().is_empty() {
            return Ok(ParsedSamplingRules::default());
        }
        // DD_TRACE_SAMPLING_RULES is expected to be a JSON array of SamplingRuleConfig objects.
        let rules_vec: Vec<SamplingRuleConfig> = serde_json::from_str(s)?;
        Ok(ParsedSamplingRules { rules: rules_vec })
    }
}

impl Display for ParsedSamplingRules {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{}",
            serde_json::to_string(&self.rules).unwrap_or_default()
        )
    }
}
