// Copyright 2023-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use crate::stacktrace::StackFrame;
use crate::telemetry::TelemetryCrashUploader;
use crate::CrashtrackerConfiguration;
use anyhow::Context;
#[cfg(unix)]
use blazesym::symbolize::{Process, Source, Symbolizer};
use chrono::{DateTime, Utc};
use ddcommon::tag::Tag;
use serde::{Deserialize, Serialize};
use std::io::BufRead;
use std::{collections::HashMap, fs::File, io::BufReader};
use uuid::Uuid;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CrashtrackerMetadata {
    pub profiling_library_name: String,
    pub profiling_library_version: String,
    pub family: String,
    // Should include "service", "environment", etc
    pub tags: Vec<Tag>,
}

impl CrashtrackerMetadata {
    pub fn new(
        profiling_library_name: String,
        profiling_library_version: String,
        family: String,
        tags: Vec<Tag>,
    ) -> Self {
        Self {
            profiling_library_name,
            profiling_library_version,
            family,
            tags,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SigInfo {
    pub signum: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[serde(default)]
    pub signame: Option<String>,
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
    #[serde(skip_serializing_if = "Option::is_none")]
    #[serde(default)]
    pub metadata: Option<CrashtrackerMetadata>,
    pub os_info: os_info::Info,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[serde(default)]
    pub siginfo: Option<SigInfo>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    #[serde(default)]
    pub stacktrace: Vec<StackFrame>,
    pub incomplete: bool,
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
    pub fn resolve_names(&mut self, src: &Source) -> anyhow::Result<()> {
        let symbolizer = Symbolizer::new();
        for frame in &mut self.stacktrace {
            // Resolving names is best effort, just print the error and continue
            frame
                .resolve_names(src, &symbolizer)
                .unwrap_or_else(|err| eprintln!("Error resolving name {err}"));
        }
        Ok(())
    }

    pub fn resolve_names_from_process(&mut self, pid: u32) -> anyhow::Result<()> {
        let mut process = Process::new(pid.into());
        // https://github.com/libbpf/blazesym/issues/518
        process.map_files = false;
        let src = Source::Process(process);
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
            siginfo: None,
            stacktrace: vec![],
            tags: HashMap::new(),
            timestamp: None,
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

    pub fn set_siginfo(&mut self, siginfo: SigInfo) -> anyhow::Result<()> {
        anyhow::ensure!(self.siginfo.is_none());
        self.siginfo = Some(siginfo);
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
}

impl CrashInfo {
    /// Emit the CrashInfo as structured json in file `path`.
    /// SIGNAL SAFETY:
    ///     I believe but have not verified this is signal safe.
    pub fn to_file(&self, path: &str) -> anyhow::Result<()> {
        let file = File::create(path).with_context(|| format!("Failed to create {path}"))?;
        serde_json::to_writer_pretty(file, self)
            .with_context(|| format!("Failed to write json to {path}"))?;
        Ok(())
    }

    pub fn upload_to_endpoint(&self, config: &CrashtrackerConfiguration) -> anyhow::Result<()> {
        // If we're debugging to a file, dump the actual crashinfo into a json
        if let Some(endpoint) = &config.endpoint {
            if Some("file") == endpoint.url.scheme_str() {
                self.to_file(
                    endpoint
                        .url
                        .path_and_query()
                        .ok_or_else(|| anyhow::format_err!("empty path for upload to file"))?
                        .as_str(),
                )?;
            }
        }
        self.upload_to_telemetry(config)
    }

    fn upload_to_telemetry(&self, config: &CrashtrackerConfiguration) -> anyhow::Result<()> {
        if let Some(metadata) = &self.metadata {
            if let Ok(uploader) = TelemetryCrashUploader::new(metadata, config) {
                uploader.upload_to_telemetry(self, config.timeout)?;
            }
        }
        Ok(())
    }
}
