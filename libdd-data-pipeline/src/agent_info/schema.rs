// Copyright 2024-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0
//! This module provides struct representing the info endpoint response
use libdd_trace_obfuscation::{obfuscation_config, replacer::ReplaceRule};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// Wrapper for an agent info response storing the state hash from the agent
#[derive(Clone, Deserialize, Default, Debug, PartialEq)]
pub struct AgentInfo {
    /// Hash of the info
    pub state_hash: String,
    /// Info response from the agent
    pub info: AgentInfoStruct,
}

/// Schema of an agent info response
#[allow(missing_docs)]
#[derive(Clone, Serialize, Deserialize, Default, Debug, PartialEq)]
pub struct AgentInfoStruct {
    /// Version of the agent
    pub version: Option<String>,
    /// Commit of the version of the agent
    pub git_commit: Option<String>,
    /// List of available endpoints
    pub endpoints: Option<Vec<String>>,
    /// List of feature flags
    #[serde(default)]
    pub feature_flags: Vec<String>,
    pub client_drop_p0s: Option<bool>,
    pub span_meta_structs: Option<bool>,
    pub long_running_spans: Option<bool>,
    pub evp_proxy_allowed_headers: Option<Vec<String>>,
    /// Configuration of the agent
    pub config: Option<Config>,
    /// List of keys mapped to peer tags
    pub peer_tags: Option<Vec<String>>,
    /// List of span kinds eligible for stats computation
    pub span_kinds_stats_computed: Option<Vec<String>>,
    /// Obfuscation version supported by the agent for client-side stats
    pub obfuscation_version: Option<u32>,
    /// Container tags hash from HTTP response header
    pub container_tags_hash: Option<String>,
    /// Exact-match tag filters applied before stats computation (root span only).
    #[serde(default)]
    pub filter_tags: FilterTagsConfig,
    /// Regex-match tag filters applied before stats computation (root span only).
    #[serde(default)]
    pub filter_tags_regex: FilterTagsConfig,
    /// Regex patterns for root-span resource names; matching traces are excluded from stats.
    #[serde(default)]
    pub ignore_resources: Vec<String>,
}

/// Require/reject lists for tag-based trace filters exposed by the agent /info endpoint.
#[derive(Clone, Serialize, Deserialize, Default, Debug, PartialEq)]
pub struct FilterTagsConfig {
    /// All listed filters must match at least one root-span tag for the trace to be accepted.
    #[serde(default)]
    pub require: Vec<String>,
    /// If any listed filter matches a root-span tag the trace is rejected.
    #[serde(default)]
    pub reject: Vec<String>,
}

#[allow(missing_docs)]
#[derive(Clone, Serialize, Deserialize, Default, Debug, PartialEq)]
pub struct Config {
    pub default_env: Option<String>,
    pub target_tps: Option<f64>,
    pub max_eps: Option<f64>,
    pub receiver_port: Option<i32>,
    pub receiver_socket: Option<String>,
    pub connection_limit: Option<i32>,
    pub receiver_timeout: Option<i32>,
    pub max_request_bytes: Option<i64>,
    pub statsd_port: Option<i32>,
    pub max_memory: Option<f64>,
    pub max_cpu: Option<f64>,
    pub analyzed_spans_by_service: Option<HashMap<String, HashMap<String, f64>>>,
    pub obfuscation: Option<ObfuscationConfig>,
}

#[allow(missing_docs)]
#[derive(Clone, Serialize, Deserialize, Default, Debug, PartialEq)]
#[serde(default)]
pub struct ObfuscationConfig {
    // Old format from the agent, now present under sql->obfuscation_mode directly
    pub sql_obfuscation_mode: obfuscation_config::SqlObfuscationMode,
    pub remove_stack_traces: bool,
    pub sql: Option<obfuscation_config::SqlConfig>,
    pub http: obfuscation_config::HttpConfig,
    pub redis: obfuscation_config::RedisConfig,
    pub valkey: obfuscation_config::RedisConfig,
    pub credit_cards: obfuscation_config::CreditCardConfig,
    pub memcached: obfuscation_config::MemcachedConfig,
    pub elasticsearch: libdd_trace_obfuscation::json::JsonObfuscator,
    pub opensearch: libdd_trace_obfuscation::json::JsonObfuscator,
    pub mongodb: libdd_trace_obfuscation::json::JsonObfuscator,
    pub tag_replace_rules: Option<Vec<ReplaceRule>>,
}

impl AgentInfo {
    /// Return true if the agent advertises support for the extended (15 KB) resource length limit.
    pub fn is_big_resource_enabled(&self) -> bool {
        self.info
            .feature_flags
            .iter()
            .any(|flag| flag == "big_resource")
    }
}

impl From<ObfuscationConfig> for libdd_trace_obfuscation::obfuscation_config::ObfuscationConfig {
    fn from(value: ObfuscationConfig) -> Self {
        let sql_config = match value.sql {
            Some(sql_config) => sql_config,
            // Fallback for the previous /info config format
            None => obfuscation_config::SqlConfig {
                obfuscation_mode: value.sql_obfuscation_mode,
                ..Default::default()
            },
        };
        Self {
            tag_replace_rules: value.tag_replace_rules,
            http: value.http,
            memcached: value.memcached,
            redis: value.redis,
            valkey: value.valkey,
            credit_cards: value.credit_cards,
            sql: sql_config,
            elasticsearch: value.elasticsearch,
            opensearch: value.opensearch,
            mongodb: value.mongodb,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::AgentInfoStruct;
    #[duplicate::duplicate_item(
        test_name input;
        [parse_empty] ["{}"];
        [parse_old_obfuscation_config] [r#"{
            "config": {
                "obfuscation": {
                    "elastic_search": true,
                    "mongo": true,
                    "sql_exec_plan": false,
                    "sql_exec_plan_normalize": false,
                    "http": {
                        "remove_query_string": false,
                        "remove_path_digits": false
                    },
                    "remove_stack_traces": false,
                    "redis": {
                        "Enabled": true,
                        "RemoveAllArgs": false
                    },
                    "memcached": {
                        "Enabled": true,
                        "KeepCommand": false
                    }
                }
            }
        }"#];
        [parse_new_obfuscation_config] [r#"{
            "config": {
                "obfuscation": {
                    "elastic_search": true,
                    "mongo": true,
                    "sql_exec_plan": false,
                    "sql_exec_plan_normalize": false,
                    "http": {
                        "remove_query_string": false,
                        "remove_path_digits": false
                    },
                    "remove_stack_traces": false,
                    "redis": {
                        "enabled": true,
                        "remove_all_args": false
                    },
                    "memcached": {
                        "enabled": true,
                        "keep_command": false
                    }
                }
            }
        }"#];
        [parse_filter_tags] [r#"{
            "filter_tags": {
                "reject": [
                  "appsec.events.system_tests_appsec_event.value:tf-reject-exact"
                ],
                "require": [
                  "appsec.events.system_tests_appsec_event.value:tf-require-exact"
                ]
            },
            "filter_tags_regex": {
                "reject": [
                  "appsec.events.system_tests_appsec_event.value:tf-reject-regex-.*"
                ],
                "require": [
                  "appsec.events.system_tests_appsec_event.value:tf-require-regex-.*"
                ]
            },
            "ignore_resources": [
                ".*(stats-unique|StatsUniqueHandler).*"
            ]
        }"#]
    )]
    #[test]
    fn test_name() {
        let _info: AgentInfoStruct = serde_json::from_str(input)
            .expect("AgentInfoStruct should be parsed successfully from input");
    }
}
