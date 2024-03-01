// Unless explicitly stated otherwise all files in this repository are licensed under the Apache License Version 2.0.
// This product includes software developed at Datadog (https://www.datadoghq.com/). Copyright 2023-Present Datadog, Inc.
use crate::stacktrace::StackFrame;
use crate::CrashtrackerMetadata;
use anyhow::Context;
use blazesym::symbolize::{Process, Source, Symbolizer};
use chrono::{DateTime, Utc};
use datadog_profiling::exporter::{self, Endpoint, Tag};
use serde::{Deserialize, Serialize};
use std::io::BufRead;
use std::time::Duration;
use std::{collections::HashMap, fs::File, io::BufReader};
use uuid::Uuid;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SigInfo {
    pub signum: u64,
    pub signame: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CrashInfo {
    pub additional_stacktraces: HashMap<String, Vec<StackFrame>>,
    pub counters: HashMap<String, i64>,
    pub files: HashMap<String, Vec<String>>,
    pub metadata: Option<CrashtrackerMetadata>,
    pub os_info: os_info::Info,
    pub siginfo: Option<SigInfo>,
    pub stacktrace: Vec<StackFrame>,
    pub incomplete: bool,
    /// Any additional data goes here
    pub tags: HashMap<String, String>,
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

    pub fn set_stacktrace(&mut self, stacktrace: Vec<StackFrame>) -> anyhow::Result<()> {
        anyhow::ensure!(self.stacktrace.is_empty());
        self.stacktrace = stacktrace;
        Ok(())
    }

    pub fn set_timestamp_to_now(&mut self) -> anyhow::Result<()> {
        anyhow::ensure!(self.timestamp.is_none());
        self.timestamp = Some(Utc::now());
        Ok(())
    }
}

impl CrashInfo {
    /// Emit the CrashInfo as structured json in file `path`.
    /// SIGNAL SAFETY:
    ///     I believe but have not verified this is signal safe.
    pub fn to_file(&self, path: &str) -> anyhow::Result<()> {
        let file = File::create(path)?;
        serde_json::to_writer_pretty(file, self)?;
        Ok(())
    }

    /// Package the CrashInfo as a json file `crash_info.json` associated with
    /// an empty profile, and upload it to the profiling endpoint given in
    /// `endpoint`.
    /// SIGNAL SAFETY:
    ///     Uploading the data involve both allocation and synchronization and
    ///     should not be done inside a signal handler.
    pub fn upload_to_dd(
        &self,
        endpoint: Endpoint,
        timeout: Duration,
    ) -> anyhow::Result<hyper::Response<hyper::Body>> {
        fn make_tag(key: &str, value: &str) -> anyhow::Result<Tag> {
            match Tag::new(key, value) {
                Ok(tag) => Ok(tag),
                Err(e) => anyhow::bail!("{}", e),
            }
        }

        let data = serde_json::to_vec(self)?;
        let metadata = &self.metadata.as_ref().context("Missing metadata")?;

        let is_crash_tag = make_tag("is_crash", "yes")?;
        let tags = Some(
            metadata
                .tags
                .iter()
                .cloned()
                .chain([is_crash_tag])
                .collect(),
        );
        let time = Utc::now();
        let crash_file = exporter::File {
            name: "crash-info.json",
            bytes: &data,
        };
        let exporter = exporter::ProfileExporter::new(
            metadata.profiling_library_name.clone(),
            metadata.profiling_library_version.clone(),
            metadata.family.clone(),
            tags,
            endpoint,
        )?;
        let request = exporter.build(
            time,
            time,
            &[crash_file],
            &[],
            None,
            None,
            None,
            None,
            timeout,
        )?;
        let response = exporter.send(request, None)?;
        //TODO, do we need to wait a bit for the agent to finish upload?
        Ok(response)
    }

    pub fn upload_to_endpoint(
        &self,
        endpoint: Endpoint,
        timeout: Duration,
    ) -> anyhow::Result<Option<hyper::Response<hyper::Body>>> {
        // Using scheme "file" currently fails:
        // error trying to connect: Unsupported scheme file
        // Instead, manually support it.
        if Some("file") == endpoint.url.scheme_str() {
            self.to_file(
                endpoint
                    .url
                    .path_and_query()
                    .ok_or_else(|| anyhow::format_err!("empty path for upload to file"))?
                    .as_str(),
            )?;
            Ok(None)
        } else {
            Ok(Some(self.upload_to_dd(endpoint, timeout)?))
        }
    }
}
