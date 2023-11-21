// Unless explicitly stated otherwise all files in this repository are licensed under the Apache License Version 2.0.
// This product includes software developed at Datadog (https://www.datadoghq.com/). Copyright 2023-Present Datadog, Inc.
use crate::crashtracker::Metadata;
use serde::{Deserialize, Serialize};
use std::io::BufRead;
use std::{collections::HashMap, fs::File, io::BufReader};
use uuid::Uuid;

/// All fields are hex encoded integers.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StackFrame {
    ip: Option<String>,
    module_base_address: Option<String>,
    sp: Option<String>,
    symbol_address: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SigInfo {
    signum: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CrashInfo {
    counters: HashMap<String, i64>,
    files: HashMap<String, Vec<String>>,
    metadata: Metadata,
    os_info: os_info::Info,
    siginfo: Option<SigInfo>,
    stacktrace: Vec<StackFrame>,
    uuid: Uuid,
}

impl CrashInfo {
    pub fn crash_seen(&self) -> bool {
        self.siginfo.is_some()
    }
}

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
            uuid,
        }
    }

    pub fn add_counter(&mut self, name: &str, val: i64) -> anyhow::Result<()> {
        let old = self.counters.insert(name.to_string(), val);
        anyhow::ensure!(old.is_none(), "Double insert of counter {name}");
        Ok(())
    }

    pub fn add_file(&mut self, filename: &str) -> anyhow::Result<()> {
        let file = File::open(filename)?;
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
}
