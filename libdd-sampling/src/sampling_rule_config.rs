// Copyright 2025-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fmt::Display;
use std::ops::Deref;
use std::str::FromStr;

/// Configuration for a single sampling rule
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
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

    /// Tags that must match (key-value pairs).
    ///
    /// Accepts either the map shape `{"env": "prod"}` or the Remote Config
    /// wire shape `[{"key": "env", "value_glob": "prod"}]`. Internally both
    /// normalize to the map shape; the list-shape entries are required to
    /// have both `key` and `value_glob` (missing either rejects the rule).
    #[serde(default, deserialize_with = "deserialize_tags")]
    pub tags: HashMap<String, String>,

    /// Where this rule comes from (customer, dynamic, default).
    /// Not exposed in the public `datadog-opentelemetry` API — set automatically
    /// during conversion from the public `SamplingRuleConfig` type.
    #[serde(default = "default_provenance")]
    pub provenance: String,
}

impl Default for SamplingRuleConfig {
    fn default() -> Self {
        // Keep `Default` in sync with the serde defaults so that constructing a config
        // with `..Default::default()` matches what deserialization would produce.
        Self {
            sample_rate: 0.0,
            service: None,
            name: None,
            resource: None,
            tags: HashMap::new(),
            provenance: default_provenance(),
        }
    }
}

impl Display for SamplingRuleConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", serde_json::json!(self))
    }
}

fn default_provenance() -> String {
    "default".to_string()
}

/// Deserializes the `tags` field, accepting either:
///   - map shape:  `{"env": "prod", "region": "us-east-1"}`
///   - list shape: `[{"key": "env", "value_glob": "prod"}, ...]`
///
/// A list entry missing `key` or `value_glob` produces a deserialization
/// error; we never silently drop entries because that could broaden a
/// tag-constrained sampling rule.
fn deserialize_tags<'de, D>(deserializer: D) -> Result<HashMap<String, String>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    use serde::de::{MapAccess, SeqAccess, Visitor};
    use std::fmt;

    #[derive(serde::Deserialize)]
    struct ListEntry {
        key: String,
        value_glob: String,
    }

    struct TagsVisitor;

    impl<'de> Visitor<'de> for TagsVisitor {
        type Value = HashMap<String, String>;

        fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
            f.write_str("a map of string to string or a list of {key, value_glob} objects")
        }

        fn visit_map<M>(self, mut access: M) -> Result<Self::Value, M::Error>
        where
            M: MapAccess<'de>,
        {
            let mut map = HashMap::with_capacity(access.size_hint().unwrap_or(0));
            while let Some((k, v)) = access.next_entry::<String, String>()? {
                map.insert(k, v);
            }
            Ok(map)
        }

        fn visit_seq<S>(self, mut access: S) -> Result<Self::Value, S::Error>
        where
            S: SeqAccess<'de>,
        {
            let mut map = HashMap::with_capacity(access.size_hint().unwrap_or(0));
            while let Some(entry) = access.next_element::<ListEntry>()? {
                map.insert(entry.key, entry.value_glob);
            }
            Ok(map)
        }
    }

    deserializer.deserialize_any(TagsVisitor)
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
        // `Default` matches the serde default for `provenance`.
        assert_eq!(config.provenance, "default");
    }

    #[test]
    fn test_sampling_rule_config_default_matches_serde_default() {
        // Constructing from an empty-but-valid JSON object must yield the same value
        // as `Default::default()`.
        let from_serde: SamplingRuleConfig =
            serde_json::from_str(r#"{"sample_rate": 0.0}"#).unwrap();
        assert_eq!(from_serde, SamplingRuleConfig::default());
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
    fn test_sampling_rule_config_tags_accepts_map_shape() {
        // Already supported — guard against regression.
        let json = r#"{
            "sample_rate": 0.5,
            "service": "svc",
            "tags": {"env": "prod", "region": "us-east-1"}
        }"#;
        let cfg: SamplingRuleConfig = serde_json::from_str(json).unwrap();
        assert_eq!(cfg.tags.get("env").map(String::as_str), Some("prod"));
        assert_eq!(
            cfg.tags.get("region").map(String::as_str),
            Some("us-east-1")
        );
    }

    #[test]
    fn test_sampling_rule_config_tags_accepts_rc_list_shape() {
        // Remote Config wire shape: list of {key, value_glob} entries.
        let json = r#"{
            "sample_rate": 0.5,
            "service": "svc",
            "tags": [
                {"key": "env", "value_glob": "prod"},
                {"key": "region", "value_glob": "us-east-1"}
            ]
        }"#;
        let cfg: SamplingRuleConfig = serde_json::from_str(json).unwrap();
        assert_eq!(cfg.tags.get("env").map(String::as_str), Some("prod"));
        assert_eq!(
            cfg.tags.get("region").map(String::as_str),
            Some("us-east-1")
        );
    }

    #[test]
    fn test_sampling_rule_config_tags_list_with_malformed_entry_rejects() {
        // A list entry missing `value_glob` must reject the whole rule rather
        // than silently dropping the entry — silently dropping a constraint could
        // broaden a tag-constrained rule and produce a security-relevant change
        // in sampling decisions.
        let json = r#"{
            "sample_rate": 0.5,
            "tags": [
                {"key": "env", "value_glob": "prod"},
                {"key": "region"}
            ]
        }"#;
        let result: Result<SamplingRuleConfig, _> = serde_json::from_str(json);
        assert!(result.is_err(), "expected deserialization to fail");
    }

    #[test]
    fn test_sampling_rule_config_tags_absent_defaults_to_empty() {
        let json = r#"{"sample_rate": 0.5}"#;
        let cfg: SamplingRuleConfig = serde_json::from_str(json).unwrap();
        assert!(cfg.tags.is_empty());
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
