// Copyright 2024-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use chrono::{DateTime, Utc};
use error_data::ThreadData;
use stacktrace::StackTrace;
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
        let stack = self.stack.unwrap_or(StackTrace {
            format: "Missing Stacktrace".to_string(),
            frames: vec![],
        });
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

    pub fn with_kind(&mut self, kind: ErrorKind) -> &mut Self {
        self.kind = Some(kind);
        self
    }

    pub fn with_message(&mut self, message: String) -> &mut Self {
        self.message = Some(message);
        self
    }

    pub fn with_stack(&mut self, stack: StackTrace) -> &mut Self {
        self.stack = Some(stack);
        self
    }

    pub fn with_threads(&mut self, threads: Vec<ThreadData>) -> &mut Self {
        self.threads = Some(threads);
        self
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

    pub fn with_counters(&mut self, counters: HashMap<String, i64>) -> &mut Self {
        self.counters = Some(counters);
        self
    }

    pub fn with_kind(&mut self, kind: ErrorKind) -> &mut Self {
        self.error.with_kind(kind);
        self
    }

    pub fn with_files(&mut self, files: HashMap<String, Vec<String>>) -> &mut Self {
        self.files = Some(files);
        self
    }

    pub fn with_fingerprint(&mut self, fingerprint: String) -> &mut Self {
        self.fingerprint = Some(fingerprint);
        self
    }

    pub fn with_incomplete(&mut self, incomplete: bool) -> &mut Self {
        self.incomplete = Some(incomplete);
        self
    }

    pub fn with_log_messages(&mut self, log_messages: Vec<String>) -> &mut Self {
        self.log_messages = Some(log_messages);
        self
    }

    pub fn with_message(&mut self, message: String) -> &mut Self {
        self.error.with_message(message);
        self
    }

    pub fn with_metadata(&mut self, metadata: Metadata) -> &mut Self {
        self.metadata = Some(metadata);
        self
    }

    pub fn with_os_info(&mut self, os_info: OsInfo) -> &mut Self {
        self.os_info = Some(os_info);
        self
    }

    pub fn with_os_info_this_machine(&mut self) -> &mut Self {
        self.with_os_info(::os_info::get().into())
    }

    pub fn with_proc_info(&mut self, proc_info: ProcInfo) -> &mut Self {
        self.proc_info = Some(proc_info);
        self
    }

    pub fn with_sig_info(&mut self, sig_info: SigInfo) -> &mut Self {
        self.sig_info = Some(sig_info);
        self
    }

    pub fn with_span_ids(&mut self, span_ids: Vec<Span>) -> &mut Self {
        self.span_ids = Some(span_ids);
        self
    }

    pub fn with_stack(&mut self, stack: StackTrace) -> &mut Self {
        self.error.with_stack(stack);
        self
    }

    pub fn with_threads(&mut self, threads: Vec<ThreadData>) -> &mut Self {
        self.error.with_threads(threads);
        self
    }

    pub fn with_timestamp(&mut self, timestamp: DateTime<Utc>) -> &mut Self {
        self.timestamp = Some(timestamp);
        self
    }

    pub fn with_timestamp_now(&mut self) -> &mut Self {
        self.with_timestamp(Utc::now())
    }

    pub fn with_trace_ids(&mut self, trace_ids: Vec<Span>) -> &mut Self {
        self.trace_ids = Some(trace_ids);
        self
    }

    pub fn with_uuid(&mut self, uuid: String) -> &mut Self {
        self.uuid = Some(uuid);
        self
    }

    pub fn with_uuid_random(&mut self) -> &mut Self {
        self.with_uuid(Uuid::new_v4().to_string())
    }
}
