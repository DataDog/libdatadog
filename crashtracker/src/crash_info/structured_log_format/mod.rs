// Copyright 2024-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

mod stacktrace;
pub use stacktrace::*;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use uuid::Uuid;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum ErrorKind {
    SigBus,
    SigSegv,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum StackType {
    CrashTrackerV1,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Metadata {
    pub library_name: String,
    pub library_version: String,
    pub family: String,
    // Should include "service", "environment", etc
    pub tags: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StackFrame {}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ErrorData {
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub additional_stacks: HashMap<String, Vec<StackFrame>>,
    pub is_crash: bool,
    pub kind: ErrorKind,
    pub message: String,
    pub stack: Vec<StackFrame>,
    pub stack_type: StackType,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProcessInfo {
    pub pid: u32,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StructuredCrashInfo {
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub additional_stacktraces: HashMap<String, Vec<StackFrame>>,
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub counters: HashMap<String, i64>,
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub files: HashMap<String, Vec<String>>,
    pub incomplete: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub metadata: Option<CrashtrackerMetadata>,
    pub os_info: os_info::Info,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub proc_info: Option<ProcessInfo>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub span_ids: Vec<u128>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub stacktrace: Vec<StackFrame>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub trace_ids: Vec<u128>,
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub timestamp: Option<DateTime<Utc>>,
    pub uuid: Uuid,
}
