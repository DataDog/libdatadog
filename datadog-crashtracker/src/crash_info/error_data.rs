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
#[cfg(unix)]
use std::rc::Rc;

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
pub struct CachedElfResolvers<'a> {
    symbolizer: &'a mut blazesym::symbolize::Symbolizer,
    elf_resolvers: HashMap<PathBuf, Rc<ElfResolver>>,
}

#[cfg(unix)]
impl<'a> CachedElfResolvers<'a> {
    pub fn new(symbolizer: &'a mut blazesym::symbolize::Symbolizer) -> Self {
        Self {
            symbolizer,
            elf_resolvers: HashMap::new(),
        }
    }

    pub fn get(&mut self, file_path: &PathBuf) -> anyhow::Result<Rc<ElfResolver>> {
        use anyhow::Context;
        if !self.elf_resolvers.contains_key(file_path.as_path()) {
            let resolver = Rc::new(ElfResolver::open(file_path).with_context(|| {
                format!(
                    "ElfResolver::open failed for '{}'",
                    file_path.to_string_lossy()
                )
            })?);
            let _ = self
                .symbolizer
                .register_elf_resolver(file_path.as_path(), Rc::clone(&resolver));
            self.elf_resolvers.insert(file_path.clone(), resolver);
        }
        self.elf_resolvers
            .get(file_path.as_path())
            .with_context(|| "key '{}' not found in ElfResolver cache")
            .cloned()
    }
}

#[cfg(unix)]
impl ErrorData {
    pub fn normalize_ips(&mut self, pid: u32) -> anyhow::Result<()> {
        let mut symbolizer = blazesym::symbolize::Symbolizer::new();
        let mut elf_resolvers = CachedElfResolvers::new(&mut symbolizer);
        let normalizer = blazesym::normalize::Normalizer::builder()
            .enable_vma_caching(true)
            .enable_build_ids(true)
            .enable_build_id_caching(true)
            .build();
        self.normalize_ips_impl(pid, &normalizer, &mut elf_resolvers)
    }

    pub(crate) fn normalize_ips_impl(
        &mut self,
        pid: u32,
        normalizer: &blazesym::normalize::Normalizer,
        elf_resolvers: &mut CachedElfResolvers,
    ) -> anyhow::Result<()> {
        let mut errors = 0;
        self.stack
            .normalize_ips(normalizer, pid.into(), elf_resolvers)
            .unwrap_or_else(|_| errors += 1);

        for thread in &mut self.threads {
            thread
                .stack
                .normalize_ips(normalizer, pid.into(), elf_resolvers)
                .unwrap_or_else(|_| errors += 1);
        }
        anyhow::ensure!(
            errors == 0,
            "Failed to normalize ips, see frame comments for details"
        );
        Ok(())
    }

    pub(crate) fn create_symbolizer_source<'a>(
        pid: u32,
    ) -> blazesym::symbolize::source::Source<'a> {
        let mut process = blazesym::symbolize::source::Process::new(pid.into());
        // https://github.com/libbpf/blazesym/issues/518
        process.map_files = false;
        blazesym::symbolize::source::Source::Process(process)
    }

    pub(crate) fn create_normalizer() -> blazesym::normalize::Normalizer {
        blazesym::normalize::Normalizer::builder()
            .enable_vma_caching(true)
            .enable_build_ids(true)
            .enable_build_id_caching(true)
            .build()
    }

    pub fn resolve_names(&mut self, pid: u32) -> anyhow::Result<()> {
        let src = Self::create_symbolizer_source(pid);
        let symbolizer = blazesym::symbolize::Symbolizer::new();
        self.resolve_names_impl(&symbolizer, &src)
    }

    pub(crate) fn resolve_names_impl(
        &mut self,
        symbolizer: &blazesym::symbolize::Symbolizer,
        src: &blazesym::symbolize::source::Source,
    ) -> anyhow::Result<()> {
        let mut errors = 0;
        self.stack
            .resolve_names(src, symbolizer)
            .unwrap_or_else(|_| errors += 1);

        for thread in &mut self.threads {
            thread
                .stack
                .resolve_names(src, symbolizer)
                .unwrap_or_else(|_| errors += 1);
        }
        anyhow::ensure!(
            errors == 0,
            "Failed to resolve names, see frame comments for details"
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
