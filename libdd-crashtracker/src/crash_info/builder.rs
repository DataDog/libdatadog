// Copyright 2024-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use crate::runtime_callback::RuntimeStack;

use chrono::{DateTime, Utc};
use error_data::ThreadData;
use stacktrace::StackTrace;
use std::io::{BufRead, BufReader};
use unknown_value::UnknownValue;
use uuid::Uuid;

use super::*;

#[derive(Debug, Default, PartialEq)]
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

    pub fn with_kind(&mut self, kind: ErrorKind) -> anyhow::Result<()> {
        self.kind = Some(kind);
        Ok(())
    }

    pub fn with_message(&mut self, message: String) -> anyhow::Result<()> {
        self.message = Some(message);
        Ok(())
    }

    pub fn with_stack(&mut self, stack: StackTrace) -> anyhow::Result<()> {
        self.stack = Some(stack);
        Ok(())
    }

    pub fn with_stack_frame(&mut self, frame: StackFrame, incomplete: bool) -> anyhow::Result<()> {
        if let Some(stack) = &mut self.stack {
            stack.push_frame(frame, incomplete)?;
        } else {
            self.stack = Some(StackTrace::from_frames(vec![frame], incomplete));
        }
        Ok(())
    }

    pub fn with_stack_set_complete(&mut self) -> anyhow::Result<()> {
        if let Some(stack) = &mut self.stack {
            stack.set_complete()?;
        } else {
            // With https://github.com/DataDog/libdatadog/pull/1076 it happens that stack trace are
            // empty on musl based Linux (Alpine) because stack unwinding may not be able to unwind
            // passed the signal handler. This by-passing for musl is temporary and needs a fix.
            #[cfg(target_env = "musl")]
            return Ok(());
            #[cfg(not(target_env = "musl"))]
            anyhow::bail!("Can't set non-existant stack complete");
        }
        Ok(())
    }

    pub fn with_threads(&mut self, threads: Vec<ThreadData>) -> anyhow::Result<()> {
        self.threads = Some(threads);
        Ok(())
    }
}

#[derive(Debug, PartialEq)]
pub struct CrashInfoBuilder {
    pub counters: Option<HashMap<String, i64>>,
    pub error: ErrorDataBuilder,
    pub experimental: Option<Experimental>,
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
    pub uuid: Uuid,
}

impl Default for CrashInfoBuilder {
    fn default() -> Self {
        Self {
            counters: None,
            error: ErrorDataBuilder::default(),
            experimental: None,
            files: None,
            fingerprint: None,
            incomplete: None,
            log_messages: None,
            metadata: None,
            os_info: None,
            proc_info: None,
            sig_info: None,
            span_ids: None,
            timestamp: None,
            trace_ids: None,
            uuid: Uuid::new_v4(),
        }
    }
}

impl CrashInfoBuilder {
    pub fn build(self) -> anyhow::Result<CrashInfo> {
        let counters = self.counters.unwrap_or_default();
        let data_schema_version = CrashInfo::current_schema_version().to_string();
        let (error, incomplete_error) = self.error.build()?;
        let experimental = self.experimental;
        let files = self.files.unwrap_or_default();
        let fingerprint = self.fingerprint;
        let incomplete = incomplete_error || self.incomplete.unwrap_or(false);
        let log_messages = self.log_messages.unwrap_or_default();
        let metadata = self.metadata.unwrap_or_else(Metadata::unknown_value);
        let os_info = self.os_info.unwrap_or_else(OsInfo::unknown_value);
        let proc_info = self.proc_info;
        let sig_info = self.sig_info;
        let span_ids = self.span_ids.unwrap_or_default();
        let timestamp = self.timestamp.unwrap_or_else(Utc::now).to_string();
        let trace_ids = self.trace_ids.unwrap_or_default();
        let uuid = self.uuid;
        Ok(CrashInfo {
            counters,
            data_schema_version,
            error,
            experimental,
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
            uuid: uuid.to_string(),
        })
    }

    pub fn has_data(&self) -> bool {
        *self != Self::default()
    }

    pub fn new() -> Self {
        Self::default()
    }

    /// Inserts the given counter to the current set of counters in the builder.
    pub fn with_counter(&mut self, name: String, value: i64) -> anyhow::Result<()> {
        anyhow::ensure!(!name.is_empty(), "Empty counter name not allowed");
        if let Some(ref mut counters) = &mut self.counters {
            counters.insert(name, value);
        } else {
            self.counters = Some(HashMap::from([(name, value)]));
        }
        Ok(())
    }

    pub fn with_counters(&mut self, counters: HashMap<String, i64>) -> anyhow::Result<()> {
        self.counters = Some(counters);
        Ok(())
    }

    pub fn with_experimental_additional_tags(
        &mut self,
        additional_tags: Vec<String>,
    ) -> anyhow::Result<()> {
        if let Some(experimental) = &mut self.experimental {
            experimental.additional_tags = additional_tags;
        } else {
            self.experimental = Some(Experimental::new().with_additional_tags(additional_tags));
        }
        Ok(())
    }

    pub fn with_experimental_ucontext(&mut self, ucontext: String) -> anyhow::Result<()> {
        if let Some(experimental) = &mut self.experimental {
            experimental.ucontext = Some(ucontext);
        } else {
            self.experimental = Some(Experimental::new().with_ucontext(ucontext));
        }
        Ok(())
    }

    pub fn with_experimental_runtime_stack(
        &mut self,
        runtime_stack: RuntimeStack,
    ) -> anyhow::Result<()> {
        if let Some(experimental) = &mut self.experimental {
            experimental.runtime_stack = Some(runtime_stack);
        } else {
            self.experimental = Some(Experimental::new().with_runtime_stack(runtime_stack));
        }
        Ok(())
    }

    pub fn with_kind(&mut self, kind: ErrorKind) -> anyhow::Result<()> {
        self.error.with_kind(kind)
    }

    pub fn with_file(&mut self, filename: String) -> anyhow::Result<()> {
        let file = File::open(&filename).with_context(|| format!("filename: {filename}"))?;
        let lines: std::io::Result<Vec<_>> = BufReader::new(file).lines().collect();
        self.with_file_and_contents(filename, lines?)?;
        Ok(())
    }

    /// Appends the given file to the current set of files in the builder.
    pub fn with_file_and_contents(
        &mut self,
        filename: String,
        contents: Vec<String>,
    ) -> anyhow::Result<()> {
        if let Some(ref mut files) = &mut self.files {
            files.insert(filename, contents);
        } else {
            self.files = Some(HashMap::from([(filename, contents)]));
        }
        Ok(())
    }

    /// Sets the current set of files in the builder.
    pub fn with_files(&mut self, files: HashMap<String, Vec<String>>) -> anyhow::Result<()> {
        self.files = Some(files);
        Ok(())
    }

    pub fn with_fingerprint(&mut self, fingerprint: String) -> anyhow::Result<()> {
        anyhow::ensure!(!fingerprint.is_empty(), "Expect non-empty fingerprint");
        self.fingerprint = Some(fingerprint);
        Ok(())
    }

    pub fn with_incomplete(&mut self, incomplete: bool) -> anyhow::Result<()> {
        self.incomplete = Some(incomplete);
        Ok(())
    }

    /// Appends the given message to the current set of messages in the builder.
    pub fn with_log_message(&mut self, message: String, also_print: bool) -> anyhow::Result<()> {
        if also_print {
            eprintln!("{message}");
        }

        if let Some(ref mut messages) = &mut self.log_messages {
            messages.push(message);
        } else {
            self.log_messages = Some(vec![message]);
        }
        Ok(())
    }

    pub fn with_log_messages(&mut self, log_messages: Vec<String>) -> anyhow::Result<()> {
        self.log_messages = Some(log_messages);
        Ok(())
    }

    pub fn with_message(&mut self, message: String) -> anyhow::Result<()> {
        self.error.with_message(message)
    }

    pub fn with_metadata(&mut self, metadata: Metadata) -> anyhow::Result<()> {
        self.metadata = Some(metadata);
        Ok(())
    }

    pub fn with_os_info(&mut self, os_info: OsInfo) -> anyhow::Result<()> {
        self.os_info = Some(os_info);
        Ok(())
    }

    pub fn with_os_info_this_machine(&mut self) -> anyhow::Result<()> {
        self.with_os_info(::os_info::get().into())?;
        Ok(())
    }

    pub fn with_proc_info(&mut self, proc_info: ProcInfo) -> anyhow::Result<()> {
        self.proc_info = Some(proc_info);
        Ok(())
    }

    pub fn with_sig_info(&mut self, sig_info: SigInfo) -> anyhow::Result<()> {
        self.sig_info = Some(sig_info);
        Ok(())
    }

    pub fn with_span_id(&mut self, span_id: Span) -> anyhow::Result<()> {
        if let Some(ref mut span_ids) = &mut self.span_ids {
            span_ids.push(span_id);
        } else {
            self.span_ids = Some(vec![span_id]);
        }
        Ok(())
    }

    pub fn with_span_ids(&mut self, span_ids: Vec<Span>) -> anyhow::Result<()> {
        self.span_ids = Some(span_ids);
        Ok(())
    }

    pub fn with_stack(&mut self, stack: StackTrace) -> anyhow::Result<()> {
        self.error.with_stack(stack)
    }

    pub fn with_stack_frame(&mut self, frame: StackFrame, incomplete: bool) -> anyhow::Result<()> {
        self.error.with_stack_frame(frame, incomplete)
    }

    pub fn with_stack_set_complete(&mut self) -> anyhow::Result<()> {
        self.error.with_stack_set_complete()
    }

    pub fn with_thread(&mut self, thread: ThreadData) -> anyhow::Result<()> {
        if let Some(ref mut threads) = &mut self.error.threads {
            threads.push(thread);
        } else {
            self.error.threads = Some(vec![thread]);
        }
        Ok(())
    }

    pub fn with_threads(&mut self, threads: Vec<ThreadData>) -> anyhow::Result<()> {
        self.error.with_threads(threads)
    }

    pub fn with_timestamp(&mut self, timestamp: DateTime<Utc>) -> anyhow::Result<()> {
        self.timestamp = Some(timestamp);
        Ok(())
    }

    pub fn with_timestamp_now(&mut self) -> anyhow::Result<()> {
        self.with_timestamp(Utc::now())?;
        Ok(())
    }

    pub fn with_trace_id(&mut self, trace_id: Span) -> anyhow::Result<()> {
        if let Some(ref mut trace_ids) = &mut self.trace_ids {
            trace_ids.push(trace_id);
        } else {
            self.trace_ids = Some(vec![trace_id]);
        }
        Ok(())
    }

    pub fn with_trace_ids(&mut self, trace_ids: Vec<Span>) -> anyhow::Result<()> {
        self.trace_ids = Some(trace_ids);
        Ok(())
    }

    /// This method requires that the builder has a UUID and metadata set.
    /// Siginfo is optional for platforms that don't support it (like Windows)
    pub fn build_crash_ping(&self) -> anyhow::Result<CrashPing> {
        let sig_info = self.sig_info.clone();
        let metadata = self.metadata.clone().context("metadata is required")?;

        let mut builder = CrashPingBuilder::new(self.uuid).with_metadata(metadata);
        if let Some(sig_info) = sig_info {
            builder = builder.with_sig_info(sig_info);
        }
        builder.build()
    }

    pub fn is_ping_ready(&self) -> bool {
        // On Unix platforms, wait for both metadata and siginfo
        // On Windows, siginfo is not available, so only wait for metadata
        #[cfg(unix)]
        {
            self.metadata.is_some() && self.sig_info.is_some()
        }
        #[cfg(windows)]
        {
            self.metadata.is_some()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::crash_info::test_utils::TestInstance;

    #[test]
    fn test_crash_info_builder_to_crash_ping() {
        let sig_info = SigInfo::test_instance(42);
        let metadata = Metadata::test_instance(1);

        let mut crash_info_builder = CrashInfoBuilder::new();
        crash_info_builder.with_sig_info(sig_info.clone()).unwrap();
        crash_info_builder.with_metadata(metadata.clone()).unwrap();
        crash_info_builder.with_kind(ErrorKind::Panic).unwrap();

        let crash_ping = crash_info_builder.build_crash_ping().unwrap();

        assert!(!crash_ping.crash_uuid().is_empty());
        assert!(Uuid::parse_str(crash_ping.crash_uuid()).is_ok());
        assert_eq!(crash_ping.siginfo(), Some(&sig_info));
        assert_eq!(crash_ping.metadata(), &metadata);
        assert!(crash_ping.message().contains("crash processing started"));
    }
}
