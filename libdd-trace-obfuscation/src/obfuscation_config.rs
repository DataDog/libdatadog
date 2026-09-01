// Copyright 2023-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use serde::Deserialize;
use std::collections::HashSet;

use crate::{
    json::JsonObfuscator,
    replacer::{self, ReplaceRule},
    sql::{SqlObfuscateConfig, SqlObfuscationMode},
};

#[derive(Debug, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct MemcachedConfig {
    pub enabled: bool,
    pub keep_command: bool,
}

/// Mirrors the Datadog Agent defaults
/// see `pkg/config/schema/yaml/apm_config.yaml`
impl Default for MemcachedConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            keep_command: false,
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct CreditCardConfig {
    pub enabled: bool,
    pub luhn: bool,
    pub keep_values: HashSet<String>,
}

/// Mirrors the Datadog Agent defaults
/// see `pkg/config/schema/yaml/apm_config.yaml`
impl Default for CreditCardConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            luhn: false,
            keep_values: HashSet::new(),
        }
    }
}

pub type JsonStringTransformer = fn(&str) -> String;

#[derive(Debug, Clone, Deserialize)]
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

/// Mirrors the Datadog Agent defaults for the elasticsearch,
/// opensearch and mongodb (`:374`) obfuscators, which are all enabled by default.
/// see `pkg/config/schema/yaml/apm_config.yaml`
impl Default for JsonObfuscatorConfig {
    fn default() -> Self {
        Self::enabled()
    }
}

impl JsonObfuscatorConfig {
    #[must_use]
    pub fn disabled() -> Self {
        Self {
            enabled: false,
            keep_keys: HashSet::new(),
            transform_keys: HashSet::new(),
            transformer: None,
        }
    }

    #[must_use]
    pub fn enabled() -> Self {
        Self {
            enabled: true,
            keep_keys: HashSet::new(),
            transform_keys: HashSet::new(),
            transformer: None,
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct RedisConfig {
    pub enabled: bool,
    pub remove_all_args: bool,
}

/// Mirrors the Datadog Agent defaults for both `redis` and `valkey`
/// which share the same schema and defaults.
/// see `pkg/config/schema/yaml/apm_config.yaml`
impl Default for RedisConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            remove_all_args: false,
        }
    }
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
    pub elasticsearch: JsonObfuscator,
    pub opensearch: JsonObfuscator,
    pub mongodb: JsonObfuscator,
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
    #[cfg(not(target_arch = "wasm32"))]
    pub fn new_from_env() -> Result<Self, Box<dyn core::error::Error>> {
        use libdd_common::config::parse_env;
        use log::{debug, error};
        use std::env;

        let tag_replace_rules: Option<Vec<ReplaceRule>> = match env::var("DD_APM_REPLACE_TAGS") {
            Ok(replace_rules_str) => match replacer::parse_rules_from_string(&replace_rules_str) {
                Ok(res) => {
                    debug!("Successfully parsed DD_APM_REPLACE_TAGS: {res:?}");
                    Some(res)
                }
                Err(e) => {
                    error!("Failed to parse DD_APM_REPLACE_TAGS: {e}");
                    None
                }
            },
            Err(_) => None,
        };
        let http_remove_query_string =
            parse_env::bool("DD_APM_OBFUSCATION_HTTP_REMOVE_QUERY_STRING").unwrap_or(false);
        let http_remove_path_digits =
            parse_env::bool("DD_APM_OBFUSCATION_HTTP_REMOVE_PATHS_WITH_DIGITS").unwrap_or(false);
        let obfuscation_redis_enabled =
            parse_env::bool("DD_APM_OBFUSCATION_REDIS_ENABLED").unwrap_or(false);
        let obfuscation_redis_remove_all_args =
            parse_env::bool("DD_APM_OBFUSCATION_REDIS_REMOVE_ALL_ARGS").unwrap_or(false);

        let obfuscate_memcached =
            parse_env::bool("DD_APM_OBFUSCATION_MEMCACHED_ENABLED").unwrap_or(false);

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
