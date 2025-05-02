// Copyright 2024-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0
use super::stacktrace::StackTrace;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

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
        let mut errors = 0;
        let normalizer = blazesym::normalize::Normalizer::builder()
            .enable_vma_caching(true)
            .enable_build_ids(true)
            .enable_build_id_caching(true)
            .build();
        let pid = pid.into();
        self.stack
            .normalize_ips(&normalizer, pid)
            .unwrap_or_else(|_| errors += 1);

        for thread in &mut self.threads {
            thread
                .stack
                .normalize_ips(&normalizer, pid)
                .unwrap_or_else(|_| errors += 1);
        }
        anyhow::ensure!(
            errors == 0,
            "Failed to normalize ips, see frame comments for details"
        );
        Ok(())
    }

    pub fn resolve_names(&mut self, pid: u32) -> anyhow::Result<()> {
        let mut errors = 0;
        let mut process = blazesym::symbolize::Process::new(pid.into());
        // https://github.com/libbpf/blazesym/issues/518
        process.map_files = false;
        let src = blazesym::symbolize::Source::Process(process);
        let symbolizer = blazesym::symbolize::Symbolizer::new();
        self.stack
            .resolve_names(&src, &symbolizer)
            .unwrap_or_else(|_| errors += 1);

        for thread in &mut self.threads {
            thread
                .stack
                .resolve_names(&src, &symbolizer)
                .unwrap_or_else(|_| errors += 1);
        }
        anyhow::ensure!(
            errors == 0,
            "Failed to resolve names, see frame comments for details"
        );
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
