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
    pub thread_name: Option<String>,
    pub stack: Option<StackTrace>,
    pub threads: Option<Vec<ThreadData>>,
}

impl ErrorDataBuilder {
    pub fn build(self) -> anyhow::Result<(ErrorData, bool /* incomplete */)> {
        let incomplete = self.stack.is_none();
        let is_crash = true;
        let kind = self.kind.context("required field 'kind' missing")?;
        let message = self.message;
        let thread_name = self.thread_name;
        let source_type = SourceType::Crashtracking;
        let stack = self.stack.unwrap_or_else(StackTrace::missing);
        let threads = self.threads.unwrap_or_default();
        Ok((
            ErrorData {
                is_crash,
                kind,
                message,
                thread_name,
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

    pub fn with_thread_name(&mut self, thread_name: String) -> anyhow::Result<()> {
        if thread_name.trim().is_empty() {
            return Ok(());
        }
        self.thread_name = Some(thread_name);
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
            // No frames were received, but we got the end marker. This can happen on
            // certain platforms/contexts where stack unwinding fails to capture any
            // frames (e.g., musl-based Linux). Initialize an empty but incomplete stack
            // to indicate that stack collection did not succeed.
            self.stack = Some(StackTrace::new_incomplete());
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

    pub fn with_thread_name(&mut self, thread_name: String) -> anyhow::Result<()> {
        self.error.with_thread_name(thread_name)
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
        let had_no_frames = self.error.stack.is_none();
        self.error.with_stack_set_complete()?;
        if had_no_frames {
            self.with_log_message(
                "No native stack frames received; stack unwinding may have failed".to_string(),
                true,
            )?;
        }
        Ok(())
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

    /// This method requires that the builder has Metadata and Kind set
    pub fn build_crash_ping(&self) -> anyhow::Result<CrashPing> {
        let metadata = self.metadata.clone().context("metadata is required")?;
        let kind = self.error.kind.clone().context("kind is required")?;
        let message = self.error.message.clone();
        let sig_info = self.sig_info.clone();

        let mut builder = CrashPingBuilder::new(self.uuid)
            .with_metadata(metadata)
            .with_kind(kind);
        if let Some(sig_info) = sig_info {
            builder = builder.with_sig_info(sig_info);
        }
        if let Some(message) = message {
            builder = builder.with_custom_message(message);
        }
        builder.build()
    }

    pub fn is_ping_ready(&self) -> bool {
        self.metadata.is_some() && self.error.kind.is_some()
    }

    pub fn has_message(&self) -> bool {
        self.error.message.is_some()
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

    #[test]
    fn test_with_message() {
        let mut builder = CrashInfoBuilder::new();

        builder.with_kind(ErrorKind::UnixSignal).unwrap();
        let test_message = "Test error message".to_string();

        let result = builder.with_message(test_message.clone());
        assert!(result.is_ok());
        assert!(builder.has_message());

        // Build and verify message is present
        let sig_info = SigInfo::test_instance(42);
        builder.with_sig_info(sig_info).unwrap();
        builder.with_metadata(Metadata::test_instance(1)).unwrap();
        builder.with_kind(ErrorKind::UnixSignal).unwrap();

        let crash_ping = builder.build_crash_ping().unwrap();
        assert!(crash_ping.message().contains(&test_message));
    }

    #[test]
    fn test_has_message_empty() {
        let builder = CrashInfoBuilder::new();
        assert!(!builder.has_message());
    }

    #[test]
    fn test_has_message_after_setting() {
        let mut builder = CrashInfoBuilder::new();
        builder.with_message("test".to_string()).unwrap();
        assert!(builder.has_message());
    }

    #[test]
    fn test_message_overwrite() {
        let mut builder = CrashInfoBuilder::new();

        builder.with_message("first message".to_string()).unwrap();
        assert!(builder.has_message());

        // Overwrite with second message
        builder.with_message("second message".to_string()).unwrap();
        assert!(builder.has_message());

        // Build and verify only second message is present
        let sig_info = SigInfo::test_instance(42);
        builder.with_sig_info(sig_info).unwrap();
        builder.with_metadata(Metadata::test_instance(1)).unwrap();
        builder.with_kind(ErrorKind::UnixSignal).unwrap();

        let crash_ping = builder.build_crash_ping().unwrap();
        assert!(crash_ping.message().contains("second message"));
        assert!(!crash_ping.message().contains("first message"));
        builder.with_kind(ErrorKind::Panic).unwrap();

        let report = builder.build().unwrap();
        assert_eq!(report.error.message.as_deref(), Some("second message"));
    }

    #[test]
    fn test_message_with_special_characters() {
        let mut builder = CrashInfoBuilder::new();
        let special_message = "Error: 'panic' with \"quotes\" and\nnewlines\t\ttabs";

        builder.with_message(special_message.to_string()).unwrap();
        builder.with_sig_info(SigInfo::test_instance(42)).unwrap();
        builder.with_metadata(Metadata::test_instance(1)).unwrap();
        builder.with_kind(ErrorKind::UnixSignal).unwrap();

        let crash_ping = builder.build_crash_ping().unwrap();
        assert!(crash_ping.message().contains(special_message));
        builder.with_kind(ErrorKind::UnixSignal).unwrap();

        let report = builder.build().unwrap();
        assert_eq!(report.error.message.as_deref(), Some(special_message));
    }

    #[test]
    fn test_very_long_message() {
        let mut builder = CrashInfoBuilder::new();
        let long_message = "x".repeat(10000); // 10KB message

        builder.with_message(long_message.clone()).unwrap();
        assert!(builder.has_message());

        builder.with_sig_info(SigInfo::test_instance(42)).unwrap();
        builder.with_metadata(Metadata::test_instance(1)).unwrap();
        builder.with_kind(ErrorKind::UnixSignal).unwrap();

        let crash_ping = builder.build_crash_ping().unwrap();
        assert!(crash_ping.message().len() >= 10000);

        builder.with_kind(ErrorKind::UnixSignal).unwrap();
        let report = builder.build().unwrap();
        assert!(report.error.message.as_ref().unwrap().len() >= 10000);
    }

    #[test]
    fn test_no_frames_is_incomplete() {
        // When we receive an end stacktrace marker but no frames were collected,
        // we should create an empty incomplete stack rather than erroring.
        let mut builder = ErrorDataBuilder::new();
        assert!(builder.stack.is_none());

        // This should succeed and create an empty incomplete stack
        let result = builder.with_stack_set_complete();
        assert!(result.is_ok());

        // Verify we now have a stack that is incomplete (no frames were captured)
        assert!(builder.stack.is_some());
        let stack = builder.stack.as_ref().unwrap();
        assert!(stack.frames.is_empty());
        assert!(stack.incomplete);
    }

    #[test]
    fn test_with_stack_set_complete_with_frames() {
        // When we have frames and call set_complete, it should mark them complete
        let mut builder = ErrorDataBuilder::new();

        // Add a frame (which creates an incomplete stack)
        let frame = StackFrame::test_instance(1);
        builder.with_stack_frame(frame, true).unwrap();
        assert!(builder.stack.as_ref().unwrap().incomplete);

        // Mark complete
        builder.with_stack_set_complete().unwrap();

        // Verify stack is now complete
        let stack = builder.stack.as_ref().unwrap();
        assert_eq!(stack.frames.len(), 1);
        assert!(!stack.incomplete);
    }

    #[test]
    fn test_crash_info_builder_empty_stack_is_incomplete() {
        // When no frames were captured, the stack and CrashInfo should be marked
        // incomplete to indicate that stack collection did not succeed.
        let mut builder = CrashInfoBuilder::new();
        builder.with_kind(ErrorKind::UnixSignal).unwrap();

        // Simulate receiving BEGIN_STACKTRACE then END_STACKTRACE with no frames
        builder.with_stack_set_complete().unwrap();

        let crash_info = builder.build().unwrap();

        // The stack should be empty and incomplete (no frames were captured)
        assert!(crash_info.error.stack.frames.is_empty());
        assert!(crash_info.error.stack.incomplete);

        // The overall crash info should be marked complete
        assert!(!crash_info.incomplete);

        // A log message should be recorded noting that no frames were received
        assert!(crash_info
            .log_messages
            .iter()
            .any(|msg| msg.contains("No native stack frames received")));
    }

    #[test]
    #[cfg_attr(miri, ignore)] // os_info::get() spawns subprocess, unsupported by Miri
    fn test_with_os_info_this_machine() {
        let mut builder = CrashInfoBuilder::new();
        builder.with_kind(ErrorKind::UnixSignal).unwrap();

        builder.with_os_info_this_machine().unwrap();

        let crash_info = builder.build().unwrap();

        // Verify os_info was populated with non-empty values from the current machine
        assert!(!crash_info.os_info.architecture.is_empty());
        assert!(!crash_info.os_info.bitness.is_empty());
        assert!(!crash_info.os_info.os_type.is_empty());
        assert!(!crash_info.os_info.version.is_empty());

        // Verify that the os_info is not the "unknown" default values
        assert_ne!(
            crash_info.os_info.architecture, "unknown",
            "architecture should not be 'unknown'"
        );
        assert_ne!(
            crash_info.os_info.bitness, "unknown bitness",
            "bitness should not be 'unknown bitness'"
        );
        assert_ne!(
            crash_info.os_info.os_type, "Unknown",
            "os_type should not be 'Unknown'"
        );
        assert_ne!(
            crash_info.os_info.version, "Unknown",
            "version should not be 'Unknown'"
        );
    }
}
