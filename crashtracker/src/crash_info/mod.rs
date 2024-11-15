// Copyright 2023-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

mod metadata;
use ddcommon::Endpoint;
pub use metadata::*;
mod stacktrace;
pub use stacktrace::*;
mod telemetry;

use self::telemetry::TelemetryCrashUploader;
use anyhow::Context;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::io::BufRead;
use std::path::Path;
use std::{collections::HashMap, fs::File, io::BufReader};
use uuid::Uuid;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SigInfo {
    pub signum: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[serde(default)]
    pub signame: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[serde(default)]
    pub faulting_address: Option<usize>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProcessInfo {
    pub pid: u32,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
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
    pub os_info: os_info::Info,
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
    pub timestamp: Option<DateTime<Utc>>,
    pub uuid: Uuid,
}

/// Getters and predicates
impl CrashInfo {
    pub fn crash_seen(&self) -> bool {
        self.siginfo.is_some()
    }
}

impl Default for CrashInfo {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(unix)]
impl CrashInfo {
    pub fn normalize_ips(&mut self, pid: u32) -> anyhow::Result<()> {
        let normalizer = blazesym::normalize::Normalizer::new();
        let pid = pid.into();
        self.stacktrace.iter_mut().for_each(|frame| {
            frame
                .normalize_ip(&normalizer, pid)
                .unwrap_or_else(|err| eprintln!("Error resolving name {err}"))
        });
        Ok(())
    }

    pub fn resolve_names(&mut self, src: &blazesym::symbolize::Source) -> anyhow::Result<()> {
        let symbolizer = blazesym::symbolize::Symbolizer::new();
        for frame in &mut self.stacktrace {
            // Resolving names is best effort, just print the error and continue
            frame
                .resolve_names(src, &symbolizer)
                .unwrap_or_else(|err| eprintln!("Error resolving name {err}"));
        }
        Ok(())
    }

    pub fn resolve_names_from_process(&mut self, pid: u32) -> anyhow::Result<()> {
        let mut process = blazesym::symbolize::Process::new(pid.into());
        // https://github.com/libbpf/blazesym/issues/518
        process.map_files = false;
        let src = blazesym::symbolize::Source::Process(process);
        self.resolve_names(&src)
    }
}

/// Constructor and setters
impl CrashInfo {
    pub fn new() -> Self {
        let os_info = os_info::get();
        let uuid = Uuid::new_v4();
        Self {
            additional_stacktraces: HashMap::new(),
            counters: HashMap::new(),
            files: HashMap::new(),
            incomplete: false,
            metadata: None,
            os_info,
            proc_info: None,
            siginfo: None,
            span_ids: vec![],
            stacktrace: vec![],
            tags: HashMap::new(),
            timestamp: None,
            trace_ids: vec![],
            uuid,
        }
    }

    pub fn add_counter(&mut self, name: &str, val: i64) -> anyhow::Result<()> {
        let old = self.counters.insert(name.to_string(), val);
        anyhow::ensure!(old.is_none(), "Double insert of counter {name}");
        Ok(())
    }

    pub fn add_file(&mut self, filename: &str) -> anyhow::Result<()> {
        let file = File::open(filename).with_context(|| filename.to_string())?;
        let lines: std::io::Result<Vec<_>> = BufReader::new(file).lines().collect();
        self.add_file_with_contents(filename, lines?)?;
        Ok(())
    }

    pub fn add_file_with_contents(
        &mut self,
        filename: &str,
        lines: Vec<String>,
    ) -> anyhow::Result<()> {
        let old = self.files.insert(filename.to_string(), lines);
        anyhow::ensure!(
            old.is_none(),
            "Attempted to add file that was already there {filename}"
        );
        Ok(())
    }

    pub fn add_tag(&mut self, key: String, value: String) -> anyhow::Result<()> {
        anyhow::ensure!(
            !self.tags.contains_key(&key),
            "Already had tag with key: {key}"
        );
        self.tags.insert(key, value);
        Ok(())
    }

    pub fn set_incomplete(&mut self, incomplete: bool) -> anyhow::Result<()> {
        self.incomplete = incomplete;
        Ok(())
    }

    pub fn set_metadata(&mut self, metadata: CrashtrackerMetadata) -> anyhow::Result<()> {
        anyhow::ensure!(self.metadata.is_none());
        self.metadata = Some(metadata);
        Ok(())
    }

    pub fn set_procinfo(&mut self, proc_info: ProcessInfo) -> anyhow::Result<()> {
        anyhow::ensure!(self.proc_info.is_none());
        self.proc_info = Some(proc_info);
        Ok(())
    }

    pub fn set_siginfo(&mut self, siginfo: SigInfo) -> anyhow::Result<()> {
        anyhow::ensure!(self.siginfo.is_none());
        self.siginfo = Some(siginfo);
        Ok(())
    }
    pub fn set_span_ids(&mut self, ids: Vec<u128>) -> anyhow::Result<()> {
        anyhow::ensure!(self.span_ids.is_empty());
        self.span_ids = ids;
        Ok(())
    }

    pub fn set_stacktrace(
        &mut self,
        thread_id: Option<String>,
        stacktrace: Vec<StackFrame>,
    ) -> anyhow::Result<()> {
        if let Some(thread_id) = thread_id {
            anyhow::ensure!(!self.additional_stacktraces.contains_key(&thread_id));
            self.additional_stacktraces.insert(thread_id, stacktrace);
        } else {
            anyhow::ensure!(self.stacktrace.is_empty());
            self.stacktrace = stacktrace;
        }

        Ok(())
    }

    pub fn set_timestamp(&mut self, ts: DateTime<Utc>) -> anyhow::Result<()> {
        anyhow::ensure!(self.timestamp.is_none());
        self.timestamp = Some(ts);
        Ok(())
    }

    pub fn set_timestamp_to_now(&mut self) -> anyhow::Result<()> {
        self.set_timestamp(Utc::now())
    }

    pub fn set_trace_ids(&mut self, ids: Vec<u128>) -> anyhow::Result<()> {
        anyhow::ensure!(self.trace_ids.is_empty());
        self.trace_ids = ids;
        Ok(())
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

    pub fn upload_to_endpoint(&self, endpoint: &Option<Endpoint>) -> anyhow::Result<()> {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()?;

        rt.block_on(async { self.async_upload_to_endpoint(endpoint).await })
    }

    pub async fn async_upload_to_endpoint(
        &self,
        endpoint: &Option<Endpoint>,
    ) -> anyhow::Result<()> {
        // If we're debugging to a file, dump the actual crashinfo into a json
        if let Some(endpoint) = endpoint {
            if Some("file") == endpoint.url.scheme_str() {
                let path = ddcommon::decode_uri_path_in_authority(&endpoint.url)
                    .context("crash output file was not correctly formatted")?;
                self.to_file(&path)?;
                let new_path = path.with_extension("rfc5.json");
                let rfc5: crate::rfc5_crash_info::CrashInfo = self.clone().into();
                rfc5.to_file(&new_path)?;
            }
        }

        self.upload_to_telemetry(endpoint).await
    }

    async fn upload_to_telemetry(&self, endpoint: &Option<Endpoint>) -> anyhow::Result<()> {
        if let Some(metadata) = &self.metadata {
            if let Ok(uploader) = TelemetryCrashUploader::new(metadata, endpoint) {
                uploader.upload_to_telemetry(self).await?
            }
        }
        Ok(())
    }
}
