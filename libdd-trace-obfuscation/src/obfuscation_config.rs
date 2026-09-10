// Copyright 2023-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use serde::{Deserialize, Serialize};
use std::collections::HashSet;

use crate::{
    json::JsonObfuscator,
    replacer::{self, ReplaceRule},
};

#[derive(Clone, Serialize, Deserialize, Debug, PartialEq, Eq)]
#[serde(default)]
pub struct MemcachedConfig {
    // Agent sent pascal case fields here in versions <7.79.0
    #[serde(alias = "Enabled")]
    pub enabled: bool,
    #[serde(alias = "KeepCommand")]
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

#[derive(Clone, Serialize, Deserialize, Debug, PartialEq, Eq)]
#[serde(default)]
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

#[derive(Clone, Serialize, Deserialize, Debug)]
#[serde(default)]
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

impl PartialEq for JsonObfuscatorConfig {
    fn eq(&self, other: &Self) -> bool {
        self.enabled == other.enabled
            && self.keep_keys == other.keep_keys
            && self.transform_keys == other.transform_keys
            && self.transformer.is_none()
            && other.transformer.is_none()
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

#[derive(Clone, Serialize, Deserialize, Debug, PartialEq, Eq)]
#[serde(default)]
pub struct RedisConfig {
    // Agent sent pascal case fields here in versions <7.79.0
    #[serde(alias = "Enabled")]
    pub enabled: bool,
    #[serde(alias = "RemoveAllArgs")]
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

#[derive(Clone, Serialize, Deserialize, Default, Debug, PartialEq, Eq)]
#[serde(default)]
pub struct HttpConfig {
    pub remove_query_string: bool,
    pub remove_path_digits: bool,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct ObfuscationConfig {
    pub tag_replace_rules: Vec<ReplaceRule>,
    pub http: HttpConfig,
    pub memcached: MemcachedConfig,
    pub redis: RedisConfig,
    pub valkey: RedisConfig,
    pub credit_cards: CreditCardConfig,
    pub sql: SqlConfig,
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

        let tag_replace_rules: Vec<ReplaceRule> = match env::var("DD_APM_REPLACE_TAGS") {
            Ok(replace_rules_str) => match replacer::parse_rules_from_string(&replace_rules_str) {
                Ok(res) => {
                    debug!("Successfully parsed DD_APM_REPLACE_TAGS: {res:?}");
                    res
                }
                Err(e) => {
                    error!("Failed to parse DD_APM_REPLACE_TAGS: {e}");
                    Vec::new()
                }
            },
            Err(_) => Vec::new(),
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
                remove_path_digits: http_remove_path_digits,
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

#[derive(Debug, Default, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum DbmsKind {
    #[default]
    Generic,
    Mssql,
    Mysql,
    Postgresql,
    Oracle,
}

/// See `DbmsKind` for the list of supported DBMS.
pub struct UnknownDBMSError;

impl TryFrom<&str> for DbmsKind {
    type Error = UnknownDBMSError;

    fn try_from(value: &str) -> Result<Self, Self::Error> {
        let res = match value.to_lowercase().as_str() {
            "" => Self::Generic,
            "mssql" => Self::Mssql,
            "mysql" => Self::Mysql,
            "postgresql" => Self::Postgresql,
            "oracle" => Self::Oracle,
            _ => return Err(UnknownDBMSError),
        };
        Ok(res)
    }
}

#[allow(deprecated)]
#[derive(Debug, Default, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum SqlObfuscationMode {
    #[default]
    #[deprecated = "kept for compatibility with agent's obfuscator but has unintuitive behavior"]
    #[serde(alias = "")]
    Unspecified,
    NormalizeOnly,
    ObfuscateOnly,
    ObfuscateAndNormalize,
}

#[derive(Clone, Serialize, Deserialize, Default, Debug, PartialEq, Eq)]
#[allow(
    clippy::struct_excessive_bools,
    reason = "public config schema, should not be refactored"
)]
#[serde(default)]
pub struct SqlConfig {
    pub replace_digits: bool,
    pub keep_sql_alias: bool,
    pub dollar_quoted_func: bool,
    pub keep_null: bool,
    pub keep_boolean: bool,
    pub keep_positional_parameter: bool,
    pub keep_trailing_semicolon: bool,
    pub keep_identifier_quotation: bool,
    pub replace_bind_parameter: bool,
    pub remove_space_between_parentheses: bool,
    pub keep_json_path: bool,
    pub obfuscation_mode: SqlObfuscationMode,
}
