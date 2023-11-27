// Unless explicitly stated otherwise all files in this repository are licensed under the Apache License Version 2.0.
// This product includes software developed at Datadog (https://www.datadoghq.com/). Copyright 2023-Present Datadog, Inc.
use crate::crashtracker::Metadata;
use crate::exporter::{self, Endpoint, Tag};
use anyhow::Context;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::io::BufRead;
use std::{collections::HashMap, fs::File, io::BufReader};
use uuid::Uuid;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StackFrameNames {
    colno: Option<u32>,
    filename: Option<String>,
    lineno: Option<u32>,
    name: Option<String>,
}

/// All fields are hex encoded integers.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StackFrame {
    ip: Option<String>,
    module_base_address: Option<String>,
    names: Vec<StackFrameNames>,
    sp: Option<String>,
    symbol_address: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SigInfo {
    signum: u64,
    signame: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CrashInfo {
    counters: HashMap<String, i64>,
    files: HashMap<String, Vec<String>>,
    metadata: Metadata,
    os_info: os_info::Info,
    siginfo: Option<SigInfo>,
    stacktrace: Vec<StackFrame>,
    timestamp: Option<DateTime<Utc>>,
    uuid: Uuid,
}

/// Getters and predicates
impl CrashInfo {
    pub fn crash_seen(&self) -> bool {
        self.siginfo.is_some()
    }

    pub fn get_metadata(&self) -> &Metadata {
        &self.metadata
    }
}

/// Constructor and setters
impl CrashInfo {
    pub fn new(metadata: Metadata) -> Self {
        let os_info = os_info::get();
        let uuid = Uuid::new_v4();
        Self {
            counters: HashMap::new(),
            files: HashMap::new(),
            metadata,
            os_info,
            siginfo: None,
            stacktrace: vec![],
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
    pub fn to_file(&self, path: &str) -> anyhow::Result<()> {
        let file = File::create(path)?;
        serde_json::to_writer_pretty(file, self)?;
        Ok(())
    }

    pub fn upload_to_dd(&self, endpoint: Endpoint) -> anyhow::Result<hyper::Response<hyper::Body>> {
        //let site = "intake.profile.datad0g.com/api/v2/profile";
        //let site = "datad0g.com";
        //let api_key = std::env::var("DD_API_KEY")?;
        let data = serde_json::to_vec(self)?;
        let metadata = self.get_metadata();

        let service_tag = match Tag::new("service", "local-crash-test-upload") {
            Ok(tag) => tag,
            Err(e) => anyhow::bail!("{}", e),
        };
        let is_crash_tag = match Tag::new("is_crash", "yes") {
            Ok(tag) => tag,
            Err(e) => anyhow::bail!("{}", e),
        };
        let tags: Option<Vec<Tag>> = Some(vec![service_tag, is_crash_tag]);
        let time = Utc::now();
        // TODO make this configurable
        // Comment that this is to prevent us waiting forever and keeping the container alive forever
        let timeout = std::time::Duration::from_secs(30);
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
        let request = exporter.build(time, time, &[crash_file], &[], None, None, None, timeout)?;
        let response = exporter.send(request, None)?;
        //TODO, do we need to wait a bit for the agent to finish upload?
        Ok(response)
    }
}
