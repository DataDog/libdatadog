// Copyright 2024-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use chrono::{DateTime, Utc};
use error_data::ThreadData;
use stacktrace::StackTrace;
use std::io::{BufRead, BufReader};
use unknown_value::UnknownValue;
use uuid::Uuid;

use super::*;

#[derive(Debug, Default)]
pub struct ErrorDataBuilder {
    pub kind: Option<ErrorKind>,
    pub message: Option<String>,
    pub stack: Option<StackTrace>,
    pub threads: Option<Vec<ThreadData>>,
}

impl ErrorDataBuilder {
    pub fn build(self) -> anyhow::Result<(ErrorData, bool /* incomplete */)> {
        let incomplete = self.stack.is_none();
        let is_crash = true;
        let kind = self.kind.context("required field 'kind' missing")?;
        let message = self.message;
        let source_type = SourceType::Crashtracking;
        let stack = self.stack.unwrap_or_else(StackTrace::missing);
        let threads = self.threads.unwrap_or_default();
        Ok((
            ErrorData {
                is_crash,
                kind,
                message,
                source_type,
                stack,
                threads,
            },
            incomplete,
        ))
    }

    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_kind(&mut self, kind: ErrorKind) -> anyhow::Result<&mut Self> {
        self.kind = Some(kind);
        Ok(self)
    }

    pub fn with_message(&mut self, message: String) -> anyhow::Result<&mut Self> {
        self.message = Some(message);
        Ok(self)
    }

    pub fn with_stack(&mut self, stack: StackTrace) -> anyhow::Result<&mut Self> {
        self.stack = Some(stack);
        Ok(self)
    }

    pub fn with_stack_frame(
        &mut self,
        frame: StackFrame,
        incomplete: bool,
    ) -> anyhow::Result<&mut Self> {
        if let Some(stack) = &mut self.stack {
            stack.push_frame(frame, incomplete)?;
        } else {
            self.stack = Some(StackTrace::from_frames(vec![frame], incomplete));
        }
        Ok(self)
    }

    pub fn with_stack_set_complete(&mut self) -> anyhow::Result<&mut Self> {
        if let Some(stack) = &mut self.stack {
            stack.set_complete()?;
        } else {
            anyhow::bail!("Can't set non-existant stack complete");
        }
        Ok(self)
    }

    pub fn with_threads(&mut self, threads: Vec<ThreadData>) -> anyhow::Result<&mut Self> {
        self.threads = Some(threads);
        Ok(self)
    }
}

#[derive(Debug, Default)]
pub struct CrashInfoBuilder {
    pub counters: Option<HashMap<String, i64>>,
    pub error: ErrorDataBuilder,
    pub files: Option<HashMap<String, Vec<String>>>,
    pub fingerprint: Option<String>,
    pub incomplete: Option<bool>,
    pub log_messages: Option<Vec<String>>,
    pub metadata: Option<Metadata>,
    pub os_info: Option<OsInfo>,
    pub proc_info: Option<ProcInfo>,
    pub sig_info: Option<SigInfo>,
    pub span_ids: Option<Vec<Span>>,
    pub timestamp: Option<DateTime<Utc>>,
    pub trace_ids: Option<Vec<Span>>,
    pub uuid: Option<String>,
}

impl CrashInfoBuilder {
    pub fn build(self) -> anyhow::Result<CrashInfo> {
        let counters = self.counters.unwrap_or_default();
        let data_schema_version = CrashInfo::current_schema_version().to_string();
        let (error, incomplete_error) = self.error.build()?;
        let files = self.files.unwrap_or_default();
        let fingerprint = self.fingerprint;
        let incomplete = incomplete_error; // TODO
        let log_messages = self.log_messages.unwrap_or_default();
        let metadata = self.metadata.unwrap_or_else(Metadata::unknown_value);
        let os_info = self.os_info.unwrap_or_else(OsInfo::unknown_value);
        let proc_info = self.proc_info;
        let sig_info = self.sig_info;
        let span_ids = self.span_ids.unwrap_or_default();
        let timestamp = self.timestamp.unwrap_or_else(Utc::now).to_string();
        let trace_ids = self.trace_ids.unwrap_or_default();
        let uuid = self.uuid.unwrap_or_else(|| Uuid::new_v4().to_string());
        Ok(CrashInfo {
            counters,
            data_schema_version,
            error,
            files,
            fingerprint,
            incomplete,
            log_messages,
            metadata,
            os_info,
            proc_info,
            sig_info,
            span_ids,
            timestamp,
            trace_ids,
            uuid,
        })
    }

    pub fn new() -> Self {
        Self::default()
    }

    /// Inserts the given counter to the current set of counters in the builder.
    pub fn with_counter(&mut self, name: String, value: i64) -> anyhow::Result<&mut Self> {
        anyhow::ensure!(!name.is_empty(), "Empty counter name not allowed");
        if let Some(ref mut counters) = &mut self.counters {
            counters.insert(name, value);
        } else {
            self.counters = Some(HashMap::from([(name, value)]));
        }
        Ok(self)
    }

    pub fn with_counters(&mut self, counters: HashMap<String, i64>) -> anyhow::Result<&mut Self> {
        self.counters = Some(counters);
        Ok(self)
    }

    pub fn with_kind(&mut self, kind: ErrorKind) -> anyhow::Result<&mut Self> {
        self.error.with_kind(kind)?;
        Ok(self)
    }

    pub fn with_file(&mut self, filename: String) -> anyhow::Result<&mut Self> {
        let file = File::open(&filename).with_context(|| format!("filename: {filename}"))?;
        let lines: std::io::Result<Vec<_>> = BufReader::new(file).lines().collect();
        self.with_file_and_contents(filename, lines?)
    }

    /// Appends the given file to the current set of files in the builder.
    pub fn with_file_and_contents(
        &mut self,
        filename: String,
        contents: Vec<String>,
    ) -> anyhow::Result<&mut Self> {
        if let Some(ref mut files) = &mut self.files {
            files.insert(filename, contents);
        } else {
            self.files = Some(HashMap::from([(filename, contents)]));
        }
        Ok(self)
    }

    /// Sets the current set of files in the builder.
    pub fn with_files(&mut self, files: HashMap<String, Vec<String>>) -> anyhow::Result<&mut Self> {
        self.files = Some(files);
        Ok(self)
    }

    pub fn with_fingerprint(&mut self, fingerprint: String) -> anyhow::Result<&mut Self> {
        anyhow::ensure!(!fingerprint.is_empty(), "Expect non-empty fingerprint");
        self.fingerprint = Some(fingerprint);
        Ok(self)
    }

    pub fn with_incomplete(&mut self, incomplete: bool) -> anyhow::Result<&mut Self> {
        self.incomplete = Some(incomplete);
        Ok(self)
    }

    /// Appends the given message to the current set of messages in the builder.
    pub fn with_log_message(&mut self, message: String) -> anyhow::Result<&mut Self> {
        if let Some(ref mut messages) = &mut self.log_messages {
            messages.push(message);
        } else {
            self.log_messages = Some(vec![message]);
        }
        Ok(self)
    }

    pub fn with_log_messages(&mut self, log_messages: Vec<String>) -> anyhow::Result<&mut Self> {
        self.log_messages = Some(log_messages);
        Ok(self)
    }

    pub fn with_message(&mut self, message: String) -> anyhow::Result<&mut Self> {
        self.error.with_message(message)?;
        Ok(self)
    }

    pub fn with_metadata(&mut self, metadata: Metadata) -> anyhow::Result<&mut Self> {
        self.metadata = Some(metadata);
        Ok(self)
    }

    pub fn with_os_info(&mut self, os_info: OsInfo) -> anyhow::Result<&mut Self> {
        self.os_info = Some(os_info);
        Ok(self)
    }

    pub fn with_os_info_this_machine(&mut self) -> anyhow::Result<&mut Self> {
        self.with_os_info(::os_info::get().into())
    }

    pub fn with_proc_info(&mut self, proc_info: ProcInfo) -> anyhow::Result<&mut Self> {
        self.proc_info = Some(proc_info);
        Ok(self)
    }

    pub fn with_sig_info(&mut self, sig_info: SigInfo) -> anyhow::Result<&mut Self> {
        self.sig_info = Some(sig_info);
        Ok(self)
    }

    pub fn with_span_id(&mut self, span_id: Span) -> anyhow::Result<&mut Self> {
        if let Some(ref mut span_ids) = &mut self.span_ids {
            span_ids.push(span_id);
        } else {
            self.span_ids = Some(vec![span_id]);
        }
        Ok(self)
    }

    pub fn with_span_ids(&mut self, span_ids: Vec<Span>) -> anyhow::Result<&mut Self> {
        self.span_ids = Some(span_ids);
        Ok(self)
    }

    pub fn with_stack(&mut self, stack: StackTrace) -> anyhow::Result<&mut Self> {
        self.error.with_stack(stack)?;
        Ok(self)
    }

    pub fn with_stack_frame(
        &mut self,
        frame: StackFrame,
        incomplete: bool,
    ) -> anyhow::Result<&mut Self> {
        self.error.with_stack_frame(frame, incomplete)?;
        Ok(self)
    }

    pub fn with_stack_set_complete(&mut self) -> anyhow::Result<&mut Self> {
        self.error.with_stack_set_complete()?;
        Ok(self)
    }

    pub fn with_thread(&mut self, thread: ThreadData) -> anyhow::Result<&mut Self> {
        if let Some(ref mut threads) = &mut self.error.threads {
            threads.push(thread);
        } else {
            self.error.threads = Some(vec![thread]);
        }
        Ok(self)
    }

    pub fn with_threads(&mut self, threads: Vec<ThreadData>) -> anyhow::Result<&mut Self> {
        self.error.with_threads(threads)?;
        Ok(self)
    }

    pub fn with_timestamp(&mut self, timestamp: DateTime<Utc>) -> anyhow::Result<&mut Self> {
        self.timestamp = Some(timestamp);
        Ok(self)
    }

    pub fn with_timestamp_now(&mut self) -> anyhow::Result<&mut Self> {
        self.with_timestamp(Utc::now())
    }

    pub fn with_trace_id(&mut self, trace_id: Span) -> anyhow::Result<&mut Self> {
        if let Some(ref mut trace_ids) = &mut self.trace_ids {
            trace_ids.push(trace_id);
        } else {
            self.trace_ids = Some(vec![trace_id]);
        }
        Ok(self)
    }

    pub fn with_trace_ids(&mut self, trace_ids: Vec<Span>) -> anyhow::Result<&mut Self> {
        self.trace_ids = Some(trace_ids);
        Ok(self)
    }

    pub fn with_uuid(&mut self, uuid: String) -> anyhow::Result<&mut Self> {
        self.uuid = Some(uuid);
        Ok(self)
    }

    pub fn with_uuid_random(&mut self) -> anyhow::Result<&mut Self> {
        self.with_uuid(Uuid::new_v4().to_string())
    }
}
