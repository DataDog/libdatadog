// Copyright 2024-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0
//! This module provides struct representing the info endpoint response
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
    pub feature_flags: Option<Vec<String>>,
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
    /// Container tags hash from HTTP response header
    pub container_tags_hash: Option<String>,
    /// Exact-match tag filters applied before stats computation (root span only).
    pub filter_tags: Option<FilterTagsConfig>,
    /// Regex-match tag filters applied before stats computation (root span only).
    pub filter_tags_regex: Option<FilterTagsConfig>,
    /// Regex patterns for root-span resource names; matching traces are excluded from stats.
    pub ignore_resources: Option<Vec<String>>,
}

/// Require/reject lists for tag-based trace filters exposed by the agent /info endpoint.
#[derive(Clone, Serialize, Deserialize, Default, Debug, PartialEq)]
pub struct FilterTagsConfig {
    /// All listed filters must match at least one root-span tag for the trace to be accepted.
    pub require: Option<Vec<String>>,
    /// If any listed filter matches a root-span tag the trace is rejected.
    pub reject: Option<Vec<String>>,
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
}

#[allow(missing_docs)]
#[derive(Clone, Deserialize, Default, Debug, PartialEq)]
pub struct ObfuscationConfig {
    pub elastic_search: bool,
    pub mongo: bool,
    pub sql_exec_plan: bool,
    pub sql_exec_plan_normalize: bool,
    pub http: HttpObfuscationConfig,
    pub remove_stack_traces: bool,
    pub redis: RedisObfuscationConfig,
    pub memcached: MemcachedObfuscationConfig,
}

#[allow(missing_docs)]
#[derive(Clone, Deserialize, Default, Debug, PartialEq)]
pub struct HttpObfuscationConfig {
    pub remove_query_string: bool,
    pub remove_path_digits: bool,
}

#[allow(missing_docs)]
#[derive(Clone, Deserialize, Default, Debug, PartialEq)]
pub struct RedisObfuscationConfig {
    pub enabled: bool,
    pub remove_all_args: bool,
}

#[allow(missing_docs)]
#[derive(Clone, Deserialize, Default, Debug, PartialEq)]
pub struct MemcachedObfuscationConfig {
    pub enabled: bool,
    pub keep_command: bool,
}
