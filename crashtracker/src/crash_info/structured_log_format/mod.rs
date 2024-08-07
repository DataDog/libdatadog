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

use anyhow::Context;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::{collections::HashMap, fs::File, path::Path};
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
        let (message, kind) = if let Some(siginfo) = value.siginfo {
            (
                siginfo.signame.unwrap_or_default(),
                match siginfo.signum as libc::c_int {
                    libc::SIGSEGV => ErrorKind::SigSegv,
                    libc::SIGBUS => ErrorKind::SigBus,
                    _ => ErrorKind::Unknown,
                },
            )
        } else {
            ("Unknown".to_string(), ErrorKind::Unknown)
        };

        let additional_stacks = value
            .additional_stacktraces
            .into_iter()
            .map(|(k, v)| (k, v.into()))
            .collect();

        let error_data = ErrorData {
            additional_stacks,
            is_crash: true,
            kind,
            message,
            stack: value.stacktrace.into(),
            stack_type: StackType::CrashTrackerV1,
        };

        Self {
            counters: value.counters,
            error: error_data,
            files: value.files,
            incomplete: value.incomplete,
            metadata: value.metadata.map(Metadata::from),
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

impl StructuredCrashInfo {
    /// Emit the CrashInfo as structured json in file `path`.
    /// SIGNAL SAFETY:
    ///     I believe but have not verified this is signal safe.
    pub fn to_file(&self, path: &Path) -> anyhow::Result<()> {
        let file =
            File::create(path).with_context(|| format!("Failed to create {}", path.display()))?;
        serde_json::to_writer_pretty(file, self)
            .with_context(|| format!("Failed to write json to {}", path.display()))?;
        Ok(())
    }
}
