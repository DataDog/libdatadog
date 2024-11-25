// Copyright 2024-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

mod builder;
mod error_data;
mod metadata;
mod os_info;
mod proc_info;
mod sig_info;
mod spans;
mod stacktrace;
mod unknown_value;

pub use builder::*;
pub use error_data::*;
pub use metadata::Metadata;
pub use stacktrace::*;

use anyhow::Context;
use os_info::OsInfo;
use proc_info::ProcInfo;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use sig_info::SigInfo;
use spans::Span;
use std::{collections::HashMap, fs::File, path::Path};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct CrashInfo {
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub counters: HashMap<String, i64>,
    pub data_schema_version: String,
    pub error: ErrorData,
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub files: HashMap<String, Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fingerprint: Option<String>,
    pub incomplete: bool,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub log_messages: Vec<String>,
    pub metadata: Metadata,
    pub os_info: OsInfo,
    pub proc_info: Option<ProcInfo>, //TODO, update the schema
    pub sig_info: Option<SigInfo>,   //TODO, update the schema
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub span_ids: Vec<Span>,
    pub timestamp: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub trace_ids: Vec<Span>,
    pub uuid: String,
}

impl CrashInfo {
    pub fn current_schema_version() -> String {
        "1.0".to_string()
    }
}

impl From<crate::crash_info::CrashInfo> for CrashInfo {
    fn from(value: crate::crash_info::CrashInfo) -> Self {
        let counters = value.counters;
        let data_schema_version = CrashInfo::current_schema_version();
        let error = {
            let is_crash = true;
            let kind = ErrorKind::UnixSignal;
            let message = None;
            let source_type = SourceType::Crashtracking;
            let stack = value.stacktrace.into();
            let threads = thread_data_from_additional_stacktraces(value.additional_stacktraces);
            ErrorData {
                is_crash,
                kind,
                message,
                source_type,
                stack,
                threads,
            }
        };
        let files = value.files;
        let fingerprint = None;
        let incomplete = value.incomplete;
        let log_messages = vec![];
        let metadata = value.metadata.unwrap().into();
        let os_info = value.os_info.into();
        let proc_info = value.proc_info.map(ProcInfo::from);
        let sig_info = value.siginfo.map(SigInfo::from);
        let span_ids = value
            .span_ids
            .into_iter()
            .map(|s| Span {
                id: s.to_string(),
                thread_name: None,
            })
            .collect();
        let trace_ids = value
            .trace_ids
            .into_iter()
            .map(|s| Span {
                id: s.to_string(),
                thread_name: None,
            })
            .collect();
        let timestamp = value.timestamp.unwrap().to_string();
        let uuid = value.uuid.to_string();
        Self {
            counters,
            data_schema_version,
            error,
            files,
            fingerprint,
            incomplete,
            log_messages,
            metadata,
            os_info,
            proc_info,
            sig_info,
            span_ids,
            trace_ids,
            timestamp,
            uuid,
        }
    }
}

impl CrashInfo {
    /// Emit the CrashInfo as structured json in file `path`.
    pub fn to_file(&self, path: &Path) -> anyhow::Result<()> {
        let file = File::options()
            .create(true)
            .append(true)
            .open(path)
            .with_context(|| format!("Failed to create {}", path.display()))?;
        serde_json::to_writer_pretty(file, self)
            .with_context(|| format!("Failed to write json to {}", path.display()))?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[ignore]
    #[test]
    /// Utility function to print the schema.
    fn print_schema() {
        let schema = schemars::schema_for!(CrashInfo);
        println!("{}", serde_json::to_string_pretty(&schema).unwrap());
    }
}
