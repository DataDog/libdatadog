// Copyright 2024-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0
use super::stacktrace::StackTrace;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct ErrorData {
    pub is_crash: bool,
    pub kind: ErrorKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
    pub source_type: SourceType,
    pub stack: StackTrace,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub threads: Vec<ThreadData>,
}

#[cfg(unix)]
impl ErrorData {
    pub fn normalize_ips(&mut self, pid: u32) -> anyhow::Result<()> {
        let normalizer = blazesym::normalize::Normalizer::new();
        let pid = pid.into();
        // TODO, should we continue after error or just exit?
        self.stack.normalize_ips(&normalizer, pid)?;
        for thread in &mut self.threads {
            thread.stack.normalize_ips(&normalizer, pid)?;
        }
        Ok(())
    }

    pub fn resolve_names(&mut self, pid: u32) -> anyhow::Result<()> {
        let mut process = blazesym::symbolize::Process::new(pid.into());
        // https://github.com/libbpf/blazesym/issues/518
        process.map_files = false;
        let src = blazesym::symbolize::Source::Process(process);
        let symbolizer = blazesym::symbolize::Symbolizer::new();
        self.stack.resolve_names(&src, &symbolizer)?;

        for thread in &mut self.threads {
            thread.stack.resolve_names(&src, &symbolizer)?;
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub enum SourceType {
    Crashtracking,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[repr(C)]
pub enum ErrorKind {
    Panic,
    UnhandledException,
    UnixSignal,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct ThreadData {
    pub crashed: bool,
    pub name: String,
    pub stack: StackTrace,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub state: Option<String>,
}

impl From<(String, Vec<crate::StackFrame>)> for ThreadData {
    fn from(value: (String, Vec<crate::StackFrame>)) -> Self {
        let crashed = false; // Currently, only .Net uses this, and I believe they don't put the crashing thread here
        let name = value.0;
        let stack = value.1.into();
        let state = None;
        Self {
            crashed,
            name,
            stack,
            state,
        }
    }
}

pub fn thread_data_from_additional_stacktraces(
    additional_stacktraces: HashMap<String, Vec<crate::StackFrame>>,
) -> Vec<ThreadData> {
    additional_stacktraces
        .into_iter()
        .map(|x| x.into())
        .collect()
}

#[cfg(test)]
impl super::test_utils::TestInstance for ErrorData {
    fn test_instance(seed: u64) -> Self {
        Self {
            is_crash: true,
            kind: ErrorKind::UnixSignal,
            message: None,
            source_type: SourceType::Crashtracking,
            stack: StackTrace::test_instance(seed),
            threads: vec![],
        }
    }
}
