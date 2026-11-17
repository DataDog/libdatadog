// Copyright 2025-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use std::borrow::Cow;

use chrono::{DateTime, Utc};

use crate::rules_based::{Str, ufc::UniversalFlagConfig};

/// Remote configuration for the feature flagging client. It's a central piece that defines client
/// behavior.
#[derive(Debug)]
pub struct Configuration {
    /// Timestamp when configuration was fetched by the SDK.
    #[allow(dead_code)]
    pub(crate) fetched_at: DateTime<Utc>,
    /// Flags configuration.
    pub(crate) flags: UniversalFlagConfig,
}

impl Configuration {
    /// Create a new configuration from server responses.
    pub fn from_server_response(config: UniversalFlagConfig) -> Configuration {
        let now = Utc::now();

        Configuration {
            fetched_at: now,
            flags: config,
        }
    }

    pub fn created_at(&self) -> DateTime<Utc> {
        self.flags.compiled.created_at
    }

    pub fn environment(&self) -> &str {
        &self.flags.compiled.environment.name
    }

    /// Stable identifier of the feature flag environment, when supplied by the configuration.
    pub fn environment_id(&self) -> Option<&str> {
        self.flags.compiled.environment.id.as_deref()
    }

    /// Cheaply clone the environment identity for evaluation metadata that may outlive this
    /// configuration handle.
    pub fn environment_identity(&self) -> (Option<Str>, Str) {
        (
            self.flags.compiled.environment.id.clone(),
            self.flags.compiled.environment.name.clone(),
        )
    }

    pub fn observe_full_evaluation_data(&self) -> bool {
        self.flags.compiled.observe_full_evaluation_data
    }

    /// Returns an iterator over all flag keys. Note that this may return both disabled flags and
    /// flags with bad configuration. Mostly useful for debugging.
    pub fn flag_keys(&self) -> impl Iterator<Item = &Str> {
        self.flags.compiled.flags.keys()
    }

    /// Returns bytes representing flags configuration.
    ///
    /// The return value should be treated as opaque and passed on to another feature flagging
    /// client for initialization.
    pub fn get_flags_configuration(&self) -> Option<Cow<'_, [u8]>> {
        Some(Cow::Borrowed(self.flags.to_json()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn observe_full_evaluation_data_is_exposed_from_server_response() {
        let json = br#"
              {
                "createdAt": "2024-07-18T00:00:00Z",
                "environment": { "name": "test" },
                "observeFullEvaluationData": true,
                "flags": {}
              }
            "#;
        let config = Configuration::from_server_response(
            UniversalFlagConfig::from_json(json.to_vec()).unwrap(),
        );
        assert!(config.observe_full_evaluation_data());
    }

    #[test]
    fn observe_full_evaluation_data_defaults_to_false_from_server_response() {
        let json = br#"
              {
                "createdAt": "2024-07-18T00:00:00Z",
                "environment": { "name": "test" },
                "flags": {}
              }
            "#;
        let config = Configuration::from_server_response(
            UniversalFlagConfig::from_json(json.to_vec()).unwrap(),
        );
        assert!(!config.observe_full_evaluation_data());
    }

    #[test]
    fn environment_identity_is_exposed_and_legacy_id_is_optional() {
        for (environment, expected_id, expected_name) in [
            (
                r#"{ "id": "11111111-1111-4111-8111-111111111111", "name": "Production" }"#,
                Some("11111111-1111-4111-8111-111111111111"),
                "Production",
            ),
            (r#"{ "name": "Legacy" }"#, None, "Legacy"),
        ] {
            let json = format!(
                r#"{{
                    "createdAt": "2024-07-18T00:00:00Z",
                    "environment": {environment},
                    "flags": {{}}
                }}"#
            );
            let config = Configuration::from_server_response(
                UniversalFlagConfig::from_json(json.into_bytes()).unwrap(),
            );
            assert_eq!(config.environment_id(), expected_id);
            assert_eq!(config.environment(), expected_name);
        }
    }
}
