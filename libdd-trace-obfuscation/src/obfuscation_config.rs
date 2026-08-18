// Copyright 2023-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use serde::Deserialize;
use std::{collections::HashSet, env};

use libdd_capabilities::env::EnvCapability;

use crate::{
    replacer::{self, ReplaceRule},
    sql::{SqlObfuscateConfig, SqlObfuscationMode},
};

#[derive(Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct MemcachedConfig {
    pub enabled: bool,
    pub keep_command: bool,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct CreditCardConfig {
    pub enabled: bool,
    pub luhn: bool,
    pub keep_values: HashSet<String>,
}

pub type JsonStringTransformer = fn(&str) -> String;

#[derive(Debug, Default, Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct JsonObfuscatorConfig {
    pub enabled: bool,
    /// `keep_keys` will specify a set of keys for which their values will
    /// not be obfuscated.
    pub keep_keys: HashSet<String>,
    /// `transform_keys` will specify a set of keys for which their values will be transformed
    /// through `transformer`
    #[serde(skip)]
    pub transform_keys: HashSet<String>,
    /// `transformer` is an optional String -> String function which will transform values
    /// specified in `transform_keys`
    #[serde(skip)]
    pub transformer: Option<JsonStringTransformer>,
}

impl JsonObfuscatorConfig {
    #[must_use]
    pub fn disabled() -> Self {
        Self {
            enabled: false,
            ..Default::default()
        }
    }

    #[must_use]
    pub fn enabled() -> Self {
        Self {
            enabled: true,
            ..Default::default()
        }
    }
}

#[derive(Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct RedisConfig {
    pub enabled: bool,
    pub remove_all_args: bool,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct HttpConfig {
    // pub enabled: bool,
    pub remove_query_string: bool,
    pub remove_paths_with_digits: bool,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ObfuscationConfig {
    pub tag_replace_rules: Option<Vec<ReplaceRule>>,
    pub http: HttpConfig,
    pub memcached: MemcachedConfig,
    pub redis: RedisConfig,
    pub valkey: RedisConfig,
    pub credit_cards: CreditCardConfig,
    pub sql: SqlObfuscateConfig,
    pub elasticsearch: JsonObfuscatorConfig,
    pub opensearch: JsonObfuscatorConfig,
    pub mongodb: JsonObfuscatorConfig,
}

// Small subset of `ObfuscationConfig` for stats obfuscation only
#[derive(Default)]
pub struct StatsObfuscationConfig {
    pub sql_obfuscation_mode: SqlObfuscationMode,
}

impl ObfuscationConfig {
    /// Builds the obfuscation config from environment variables.
    ///
    /// # Errors
    ///
    /// Returns an error if one of the regular expressions used by the config cannot be compiled.
    pub fn new() -> Result<Self, Box<dyn core::error::Error>> {
        Self::from_getter(|name| Ok::<_, env::VarError>(env::var(name).ok()))
    }

    /// Builds the obfuscation config from a platform environment capability.
    ///
    /// # Errors
    ///
    /// Returns an error when an environment value cannot be read or a replacement rule is invalid.
    pub fn from_env<C: EnvCapability>(source: &C) -> Result<Self, Box<dyn core::error::Error>> {
        Self::from_getter(|name| source.get(name))
    }

    fn from_getter<F, E>(mut get: F) -> Result<Self, Box<dyn core::error::Error>>
    where
        F: FnMut(&str) -> Result<Option<String>, E>,
        E: core::error::Error + 'static,
    {
        let tag_replace_rules = get("DD_APM_REPLACE_TAGS")?
            .map(|rules| replacer::parse_rules_from_string(&rules))
            .transpose()?;
        let http_remove_query_string =
            enabled(get("DD_APM_OBFUSCATION_HTTP_REMOVE_QUERY_STRING")?.as_deref());
        let http_remove_path_digits =
            enabled(get("DD_APM_OBFUSCATION_HTTP_REMOVE_PATHS_WITH_DIGITS")?.as_deref());
        let obfuscation_redis_enabled =
            enabled(get("DD_APM_OBFUSCATION_REDIS_ENABLED")?.as_deref());
        let obfuscation_redis_remove_all_args =
            enabled(get("DD_APM_OBFUSCATION_REDIS_REMOVE_ALL_ARGS")?.as_deref());
        let obfuscate_memcached = enabled(get("DD_APM_OBFUSCATION_MEMCACHED_ENABLED")?.as_deref());

        Ok(Self {
            tag_replace_rules,
            http: HttpConfig {
                remove_query_string: http_remove_query_string,
                remove_paths_with_digits: http_remove_path_digits,
            },
            memcached: MemcachedConfig {
                enabled: obfuscate_memcached,
                keep_command: true,
            },
            credit_cards: CreditCardConfig {
                enabled: true,
                luhn: true,
                keep_values: HashSet::new(),
            },
            redis: RedisConfig {
                enabled: obfuscation_redis_enabled,
                remove_all_args: obfuscation_redis_remove_all_args,
            },
            ..Default::default()
        })
    }
}

fn enabled(value: Option<&str>) -> bool {
    matches!(value, Some("true" | "1"))
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use libdd_capabilities::env::{EnvCapability, EnvError};

    use super::ObfuscationConfig;

    #[derive(Clone, Debug, Default)]
    struct TestEnv(HashMap<String, String>);

    impl EnvCapability for TestEnv {
        fn new() -> Self {
            Self::default()
        }

        fn get(&self, name: &str) -> Result<Option<String>, EnvError> {
            Ok(self.0.get(name).cloned())
        }
    }

    #[derive(Clone, Debug)]
    struct BrokenEnv;

    impl EnvCapability for BrokenEnv {
        fn new() -> Self {
            Self
        }

        fn get(&self, _name: &str) -> Result<Option<String>, EnvError> {
            Err(EnvError::Io(anyhow::anyhow!("environment unavailable")))
        }
    }

    #[test]
    fn reads_from_environment_capability() {
        let source = TestEnv(HashMap::from([
            (
                "DD_APM_REPLACE_TAGS".to_owned(),
                r#"[{"name":"custom.tag","pattern":"secret","repl":"?"}]"#.to_owned(),
            ),
            (
                "DD_APM_OBFUSCATION_HTTP_REMOVE_QUERY_STRING".to_owned(),
                "true".to_owned(),
            ),
            (
                "DD_APM_OBFUSCATION_HTTP_REMOVE_PATHS_WITH_DIGITS".to_owned(),
                "1".to_owned(),
            ),
            (
                "DD_APM_OBFUSCATION_REDIS_ENABLED".to_owned(),
                "true".to_owned(),
            ),
            (
                "DD_APM_OBFUSCATION_REDIS_REMOVE_ALL_ARGS".to_owned(),
                "1".to_owned(),
            ),
            (
                "DD_APM_OBFUSCATION_MEMCACHED_ENABLED".to_owned(),
                "true".to_owned(),
            ),
        ]));

        let config = ObfuscationConfig::from_env(&source).unwrap();

        assert_eq!(config.tag_replace_rules.unwrap().len(), 1);
        assert!(config.http.remove_query_string);
        assert!(config.http.remove_paths_with_digits);
        assert!(config.redis.enabled);
        assert!(config.redis.remove_all_args);
        assert!(config.memcached.enabled);
        assert!(config.memcached.keep_command);
    }

    #[test]
    fn rejects_invalid_replacement_rules() {
        let source = TestEnv(HashMap::from([(
            "DD_APM_REPLACE_TAGS".to_owned(),
            r#"[{"name":"custom.tag","pattern":"[","repl":"?"}]"#.to_owned(),
        )]));

        assert!(ObfuscationConfig::from_env(&source).is_err());
    }

    #[test]
    fn propagates_environment_errors() {
        let error = ObfuscationConfig::from_env(&BrokenEnv).unwrap_err();

        assert_eq!(error.to_string(), "IO error: environment unavailable");
    }
}
