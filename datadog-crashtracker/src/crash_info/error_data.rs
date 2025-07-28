// Copyright 2024-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0
use super::stacktrace::StackTrace;
#[cfg(unix)]
use blazesym::helper::ElfResolver;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
#[cfg(unix)]
use std::collections::HashMap;
#[cfg(unix)]
use std::path::PathBuf;

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
#[derive(Default)]
pub struct CachedElfResolvers {
    elf_resolvers: HashMap<PathBuf, ElfResolver>,
}

#[cfg(unix)]
impl CachedElfResolvers {
    pub fn get(&mut self, file_path: &PathBuf) -> anyhow::Result<&ElfResolver> {
        use anyhow::Context;
        if !self.elf_resolvers.contains_key(file_path.as_path()) {
            let resolver = ElfResolver::open(file_path)?;
            self.elf_resolvers.insert(file_path.clone(), resolver);
        }
        self.elf_resolvers
            .get(file_path.as_path())
            .with_context(|| "key '{}' not found in ElfResolver cache")
    }
}

#[cfg(unix)]
impl ErrorData {
    pub fn enrich_stacks(&mut self, pid: u32) -> anyhow::Result<()> {
        let mut errors = 0;
        let mut elf_resolvers = CachedElfResolvers::default();
        let normalizer = blazesym::normalize::Normalizer::builder()
            .enable_vma_caching(true)
            .enable_build_ids(true)
            .enable_build_id_caching(true)
            .build();
        let mut process = blazesym::symbolize::source::Process::new(pid.into());
        // https://github.com/libbpf/blazesym/issues/518
        process.map_files = false;
        let src = blazesym::symbolize::source::Source::Process(process);
        let symbolizer = blazesym::symbolize::Symbolizer::new();
        let pid = pid.into();
        self.stack
            .enrich_frames(&normalizer, &src, &symbolizer, pid, &mut elf_resolvers)
            .unwrap_or_else(|_| errors += 1);

        for thread in &mut self.threads {
            thread
                .stack
                .enrich_frames(&normalizer, &src, &symbolizer, pid, &mut elf_resolvers)
                .unwrap_or_else(|_| errors += 1);
        }
        anyhow::ensure!(
            errors == 0,
            "Failed to enrich stacks, see frame comments for details"
        );
        Ok(())
    }
}

impl ErrorData {
    pub fn demangle_names(&mut self) -> anyhow::Result<()> {
        let mut errors = 0;
        self.stack.demangle_names().unwrap_or_else(|_| errors += 1);
        for thread in &mut self.threads {
            thread
                .stack
                .demangle_names()
                .unwrap_or_else(|_| errors += 1);
        }
        anyhow::ensure!(
            errors == 0,
            "Failed to demangle names, see frame comments for details"
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
