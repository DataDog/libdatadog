// Copyright 2023-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use anyhow::Context;
use chrono::{DateTime, Utc};
use ddcommon::tag::Tag;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::io::BufRead;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct SigInfo {
    pub signum: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[serde(default)]
    pub signame: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct ProcessInfo {
    pub pid: u32,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub enum Version {
    Unknown,
    Semantic(u64, u64, u64),
    Rolling(Option<String>),
    Custom(String),
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct OsInfo {
    architecture: String,
    bitness: String,
    os_type: String,
    version : Version,
}

#[test]
fn schema() {
    let schema = schemars::schema_for!(CrashInfo);
    println!("{}", serde_json::to_string_pretty(&schema).unwrap());
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct CrashInfo {
    #[serde(skip_serializing_if = "HashMap::is_empty")]
    #[serde(default)]
    pub additional_stacktraces: HashMap<String, Vec<StackFrame>>,
    #[serde(skip_serializing_if = "HashMap::is_empty")]
    #[serde(default)]
    pub counters: HashMap<String, i64>,
    #[serde(skip_serializing_if = "HashMap::is_empty")]
    #[serde(default)]
    pub files: HashMap<String, Vec<String>>,
    pub incomplete: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[serde(default)]
    pub metadata: Option<CrashtrackerMetadata>,
    pub os_info: OsInfo,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[serde(default)]
    pub proc_info: Option<ProcessInfo>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[serde(default)]
    pub siginfo: Option<SigInfo>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    #[serde(default)]
    pub span_ids: Vec<u128>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    #[serde(default)]
    pub stacktrace: Vec<StackFrame>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    #[serde(default)]
    pub trace_ids: Vec<u128>,
    /// Any additional data goes here
    #[serde(skip_serializing_if = "HashMap::is_empty")]
    #[serde(default)]
    pub tags: HashMap<String, String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[serde(default)]
    pub timestamp: Option<String>,
    pub uuid: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default, JsonSchema)]
pub struct StackFrameNames {
    #[serde(skip_serializing_if = "Option::is_none")]
    #[serde(default)]
    pub colno: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[serde(default)]
    pub filename: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[serde(default)]
    pub lineno: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[serde(default)]
    pub name: Option<String>,
}

/// All fields are hex encoded integers.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct StackFrame {
    #[serde(skip_serializing_if = "Option::is_none")]
    #[serde(default)]
    pub ip: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[serde(default)]
    pub module_base_address: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[serde(default)]
    pub names: Option<Vec<StackFrameNames>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[serde(default)]
    pub normalized_ip: Option<NormalizedAddress>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[serde(default)]
    pub sp: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[serde(default)]
    pub symbol_address: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub enum NormalizedAddressMeta {
    Apk(PathBuf),
    Elf {
        path: PathBuf,
        build_id: Option<Vec<u8>>,
    },
    Unknown,
    Unexpected(String),
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct NormalizedAddress {
    pub file_offset: u64,
    pub meta: NormalizedAddressMeta,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct CrashtrackerMetadata {
    pub profiling_library_name: String,
    pub profiling_library_version: String,
    pub family: String,
    // Should include "service", "environment", etc
    pub tags: Vec<Tag>,
}
