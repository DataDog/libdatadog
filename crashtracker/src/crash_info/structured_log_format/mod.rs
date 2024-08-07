// Copyright 2024-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

mod stacktrace;
pub use stacktrace::*;
mod error_data;
pub use error_data::*;
mod metadata;
pub use metadata::*;
mod process_info;
pub use process_info::*;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use uuid::Uuid;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StructuredCrashInfo {
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub counters: HashMap<String, i64>,
    pub error: ErrorData,
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub files: HashMap<String, Vec<String>>,
    pub incomplete: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub metadata: Option<Metadata>,
    pub os_info: os_info::Info,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub proc_info: Option<ProcessInfo>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub span_ids: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub trace_ids: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timestamp: Option<DateTime<Utc>>,
    pub uuid: Uuid,
    pub version_id: u64,
}

impl From<super::internal::CrashInfo> for StructuredCrashInfo {
    fn from(value: super::internal::CrashInfo) -> Self {
        let kind = if let Some(siginfo) = value.siginfo {
            match siginfo.signum as libc::c_int {
                libc::SIGSEGV => ErrorKind::SigSegv,
                libc::SIGBUS => ErrorKind::SigBus,
                _ => ErrorKind::Unknown,
            }
        } else {
            ErrorKind::Unknown
        };
        let error_data = ErrorData {
            additional_stacks: HashMap::new(),
            is_crash: true,
            kind,
            message: "placeholder".to_string(),
            stack: vec![],
            stack_type: StackType::CrashTrackerV1,
        };

        Self {
            counters: value.counters,
            error: error_data,
            files: value.files,
            incomplete: value.incomplete,
            metadata: None,         //TODO
            os_info: value.os_info, //TODO, make this defined
            proc_info: value.proc_info.map(ProcessInfo::from),
            span_ids: value.span_ids.into_iter().map(|v| v.to_string()).collect(),
            trace_ids: value.trace_ids.into_iter().map(|v| v.to_string()).collect(),
            timestamp: value.timestamp,
            uuid: value.uuid,
            version_id: 1,
        }
    }
}
