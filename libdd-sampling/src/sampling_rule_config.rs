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

#[cfg(test)]
mod tests {
    use super::*;

    // --- SamplingRuleConfig ---

    #[test]
    fn test_sampling_rule_config_defaults() {
        let config = SamplingRuleConfig::default();
        assert_eq!(config.sample_rate, 0.0);
        assert!(config.service.is_none());
        assert!(config.name.is_none());
        assert!(config.resource.is_none());
        assert!(config.tags.is_empty());
        // derive(Default) gives "" for String; "default" is only the serde deserialization default
        assert_eq!(config.provenance, "");
    }

    #[test]
    fn test_sampling_rule_config_serde_default_provenance() {
        // When provenance is absent from JSON, serde fills it in as "default"
        let json = r#"{"sample_rate": 0.5}"#;
        let config: SamplingRuleConfig = serde_json::from_str(json).unwrap();
        assert_eq!(config.provenance, "default");
    }

    #[test]
    fn test_sampling_rule_config_deserialize_full() {
        let json = r#"{
            "sample_rate": 0.5,
            "service": "my-service",
            "name": "http.*",
            "resource": "/api/*",
            "tags": {"env": "prod"},
            "provenance": "customer"
        }"#;
        let config: SamplingRuleConfig = serde_json::from_str(json).unwrap();
        assert_eq!(config.sample_rate, 0.5);
        assert_eq!(config.service.as_deref(), Some("my-service"));
        assert_eq!(config.name.as_deref(), Some("http.*"));
        assert_eq!(config.resource.as_deref(), Some("/api/*"));
        assert_eq!(config.tags.get("env").map(String::as_str), Some("prod"));
        assert_eq!(config.provenance, "customer");
    }

    #[test]
    fn test_sampling_rule_config_deserialize_minimal() {
        let json = r#"{"sample_rate": 1.0}"#;
        let config: SamplingRuleConfig = serde_json::from_str(json).unwrap();
        assert_eq!(config.sample_rate, 1.0);
        assert!(config.service.is_none());
        assert_eq!(config.provenance, "default");
    }

    #[test]
    fn test_sampling_rule_config_roundtrip() {
        let original = SamplingRuleConfig {
            sample_rate: 0.25,
            service: Some("svc".into()),
            name: Some("op".into()),
            resource: Some("/res".into()),
            tags: HashMap::from([("k".into(), "v".into())]),
            provenance: "dynamic".into(),
        };
        let json = serde_json::to_string(&original).unwrap();
        let restored: SamplingRuleConfig = serde_json::from_str(&json).unwrap();
        assert_eq!(original, restored);
    }

    #[test]
    fn test_sampling_rule_config_display() {
        let config = SamplingRuleConfig {
            sample_rate: 1.0,
            service: Some("svc".into()),
            ..Default::default()
        };
        let s = config.to_string();
        assert!(s.contains("sample_rate"));
        assert!(s.contains("svc"));
    }

    // --- ParsedSamplingRules ---

    #[test]
    fn test_parsed_sampling_rules_empty_string() {
        let parsed: ParsedSamplingRules = "".parse().unwrap();
        assert!(parsed.rules.is_empty());
    }

    #[test]
    fn test_parsed_sampling_rules_whitespace_only() {
        let parsed: ParsedSamplingRules = "   ".parse().unwrap();
        assert!(parsed.rules.is_empty());
    }

    #[test]
    fn test_parsed_sampling_rules_valid_json() {
        let json = r#"[{"sample_rate": 0.5, "service": "svc"}, {"sample_rate": 1.0}]"#;
        let parsed: ParsedSamplingRules = json.parse().unwrap();
        assert_eq!(parsed.rules.len(), 2);
        assert_eq!(parsed.rules[0].sample_rate, 0.5);
        assert_eq!(parsed.rules[1].sample_rate, 1.0);
    }

    #[test]
    fn test_parsed_sampling_rules_invalid_json() {
        let result: Result<ParsedSamplingRules, _> = "not json".parse();
        assert!(result.is_err());
    }

    #[test]
    fn test_parsed_sampling_rules_deref() {
        let json = r#"[{"sample_rate": 0.5}]"#;
        let parsed: ParsedSamplingRules = json.parse().unwrap();
        // Deref to &[SamplingRuleConfig]
        assert_eq!(parsed.len(), 1);
        assert_eq!(parsed[0].sample_rate, 0.5);
    }

    #[test]
    fn test_parsed_sampling_rules_into_vec() {
        let json = r#"[{"sample_rate": 0.5}, {"sample_rate": 1.0}]"#;
        let parsed: ParsedSamplingRules = json.parse().unwrap();
        let vec: Vec<SamplingRuleConfig> = parsed.into();
        assert_eq!(vec.len(), 2);
    }

    #[test]
    fn test_parsed_sampling_rules_display() {
        let json = r#"[{"sample_rate":0.5}]"#;
        let parsed: ParsedSamplingRules = json.parse().unwrap();
        let s = parsed.to_string();
        assert!(s.contains("sample_rate"));
        assert!(s.contains("0.5"));
    }

    #[test]
    fn test_parsed_sampling_rules_default_is_empty() {
        let parsed = ParsedSamplingRules::default();
        assert!(parsed.rules.is_empty());
    }
}
