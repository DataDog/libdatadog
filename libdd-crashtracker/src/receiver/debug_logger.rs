// Copyright 2023-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use crate::crash_info::{Metadata, TelemetryCrashUploader, UnknownValue};
use crate::CrashtrackerConfiguration;
use libdd_telemetry::data::LogLevel;
use std::sync::Arc;

#[derive(Debug)]
pub(crate) enum ReceiverIssue {
    Timeout,
    IoError,
    ProcessLine,
    AttachAdditionalFile,
    IncompleteStacktrace,
    UnexpectedLine,
    NoData,
}

impl ReceiverIssue {
    fn tag(&self) -> &'static str {
        match self {
            ReceiverIssue::Timeout => "receiver_issue:timeout",
            ReceiverIssue::IoError => "receiver_issue:io_error",
            ReceiverIssue::ProcessLine => "receiver_issue:process_line_error",
            ReceiverIssue::AttachAdditionalFile => "receiver_issue:attach_additional_file_error",
            ReceiverIssue::IncompleteStacktrace => "receiver_issue:incomplete_stacktrace",
            ReceiverIssue::UnexpectedLine => "receiver_issue:unexpected_line",
            ReceiverIssue::NoData => "receiver_issue:no_data",
        }
    }
}

/// Which of the collector's blocks the current uploader was built from.
/// Used to rebuild it when better information arrives.
#[derive(Debug, Clone, Copy, PartialEq)]
struct DebugLoggerInputs {
    config: bool,
    metadata: bool,
}

/// Uploads crashtracker debug logs to telemetry.
///
/// The receiver learns where to send and who it is speaking for incrementally:
/// the config and metadata blocks may arrive late, or not at all. Rather than
/// withholding every debug log until both are in hand, the logger is usable
/// from the first line read, built from whatever is available at the time:
///
/// - Missing metadata degrades to [`Metadata::unknown_value`]. It only supplies tags, and
///   [`TelemetryCrashUploader`] already defaults each missing tag to `unknown`.
/// - A missing config leaves the endpoint unset, so `libdd-telemetry`'s env-derived default applies
///   (`DD_TRACE_AGENT_URL`, `DD_AGENT_HOST` / `DD_TRACE_AGENT_PORT`, the APM socket, then the local
///   agent).
///
/// [`DebugLogger::update`] rebuilds the uploader once the real config and
/// metadata show up, so later logs are properly attributed and go to the
/// endpoint the collector asked for.
pub(crate) struct DebugLogger {
    uploader: Option<Arc<TelemetryCrashUploader>>,
    inputs: DebugLoggerInputs,
}

impl DebugLogger {
    pub(crate) fn new(
        config: Option<&CrashtrackerConfiguration>,
        metadata: Option<&Metadata>,
    ) -> Self {
        let inputs = DebugLoggerInputs {
            config: config.is_some(),
            metadata: metadata.is_some(),
        };
        let owned_metadata = metadata.cloned().unwrap_or_else(Metadata::unknown_value);
        let endpoint = config.and_then(|config| config.endpoint().clone());
        // A failure here leaves the logger disabled rather than aborting the
        // report: debug logs are strictly best-effort.
        let uploader = TelemetryCrashUploader::new(&owned_metadata, &endpoint)
            .ok()
            .map(Arc::new);
        Self { uploader, inputs }
    }

    /// Rebuilds the uploader if the set of available blocks has changed since
    /// it was built. A no-op if nothing new arrived.
    pub(crate) fn update(
        &mut self,
        config: Option<&CrashtrackerConfiguration>,
        metadata: Option<&Metadata>,
    ) {
        let inputs = DebugLoggerInputs {
            config: config.is_some(),
            metadata: metadata.is_some(),
        };
        if inputs != self.inputs {
            *self = Self::new(config, metadata);
        }
    }

    /// A logger that drops everything, for tests that do not exercise upload.
    /// Also avoids `tokio::spawn` outside of a runtime in synchronous tests.
    #[cfg(test)]
    pub(crate) fn disabled() -> Self {
        Self {
            uploader: None,
            inputs: DebugLoggerInputs {
                config: false,
                metadata: false,
            },
        }
    }

    fn tags(issue: ReceiverIssue, crash_uuid: &str) -> String {
        format!(
            "{},crash_uuid:{},is_crash_debug:true",
            issue.tag(),
            crash_uuid
        )
    }

    /// Sends a debug log without waiting for it. Use while the receiver still
    /// has work to do; the request completes on the runtime in the background.
    pub(crate) fn emit(
        &self,
        issue: ReceiverIssue,
        crash_uuid: &str,
        message: String,
        level: LogLevel,
    ) {
        if let Some(uploader) = self.uploader.as_ref().map(Arc::clone) {
            let tags = Self::tags(issue, crash_uuid);
            tokio::spawn(async move {
                let _ = uploader.upload_general_log(message, tags, level).await;
            });
        }
    }

    /// Sends a debug log and waits for it. Required on paths that return
    /// immediately afterwards: the caller drops the tokio runtime once the
    /// receiver is done, which cancels any still-pending spawned task.
    pub(crate) async fn emit_and_wait(
        &self,
        issue: ReceiverIssue,
        crash_uuid: &str,
        message: String,
        level: LogLevel,
    ) {
        if let Some(uploader) = self.uploader.as_ref() {
            let tags = Self::tags(issue, crash_uuid);
            let _ = uploader.upload_general_log(message, tags, level).await;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_debug_logger_usable_without_config_or_metadata() {
        // The receiver must be able to report problems before (or without) the
        // collector's config and metadata blocks, falling back to the
        // env-derived telemetry endpoint.
        let logger = DebugLogger::new(None, None);
        assert!(logger.uploader.is_some());
    }

    #[test]
    fn test_debug_logger_rebuilds_as_blocks_arrive() {
        let mut logger = DebugLogger::new(None, None);
        assert_eq!(
            logger.inputs,
            DebugLoggerInputs {
                config: false,
                metadata: false
            }
        );

        // Same inputs: keeps the uploader it already has.
        logger.update(None, None);
        assert_eq!(
            logger.inputs,
            DebugLoggerInputs {
                config: false,
                metadata: false
            }
        );

        // Metadata arrived: rebuilt so later logs are attributed to the service.
        let metadata = Metadata::unknown_value();
        logger.update(None, Some(&metadata));
        assert_eq!(
            logger.inputs,
            DebugLoggerInputs {
                config: false,
                metadata: true
            }
        );
        assert!(logger.uploader.is_some());
    }
}
