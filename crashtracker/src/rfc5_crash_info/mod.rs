// Copyright 2024-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

mod error_data;
mod metadata;
mod os_info;
mod proc_info;
mod sig_info;
mod spans;
mod stacktrace;
mod telemetry;
mod test_utils;

pub use error_data::{ErrorData, ErrorKind, SourceType};
pub use metadata::Metadata;
pub use os_info::OsInfo;
pub use proc_info::ProcInfo;
pub use sig_info::{SiCodes, SigInfo, SignalNames};
pub use spans::Span;
pub use stacktrace::{BuildIdType, FileType, StackFrame, StackTrace};
pub use telemetry::TelemetryCrashUploader;

use anyhow::Context;
use error_data::thread_data_from_additional_stacktraces;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
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
    pub proc_info: ProcInfo,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sig_info: Option<SigInfo>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub span_ids: Vec<Span>,
    pub timestamp: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub trace_ids: Vec<Span>,
    pub uuid: String,
}

impl From<crate::crash_info::CrashInfo> for CrashInfo {
    fn from(value: crate::crash_info::CrashInfo) -> Self {
        let counters = value.counters;
        let data_schema_version = String::from("1.0");
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
        let proc_info = value.proc_info.unwrap().into();
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

    impl test_utils::TestInstance for CrashInfo {
        fn test_instance(seed: u64) -> Self {
            let mut counters = HashMap::new();
            counters.insert("collecting_sample".to_owned(), 1);
            counters.insert("not_profiling".to_owned(), 0);

            let span_ids = vec![
                Span {
                    id: "42".to_string(),
                    thread_name: Some("thread1".to_string()),
                },
                Span {
                    id: "24".to_string(),
                    thread_name: Some("thread2".to_string()),
                },
            ];

            let trace_ids = vec![
                Span {
                    id: "345".to_string(),
                    thread_name: Some("thread111".to_string()),
                },
                Span {
                    id: "666".to_string(),
                    thread_name: Some("thread222".to_string()),
                },
            ];

            Self {
                counters,
                data_schema_version: "1.0".to_string(),
                error: ErrorData::test_instance(seed),
                files: HashMap::new(),
                fingerprint: None,
                incomplete: true,
                log_messages: vec![],
                metadata: Metadata::test_instance(seed),
                os_info: ::os_info::Info::unknown().into(),
                proc_info: ProcInfo::test_instance(seed),
                sig_info: Some(SigInfo::test_instance(seed)),
                span_ids,
                timestamp: chrono::DateTime::from_timestamp(1568898000 /* Datadog IPO */, 0)
                    .unwrap()
                    .to_string(),
                trace_ids,
                uuid: uuid::uuid!("1d6b97cb-968c-40c9-af6e-e4b4d71e8781").to_string(),
            }
        }
    }
}
