#![allow(dead_code)]
// Copyright 2025-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use crate::crash_info::{Metadata, TelemetryCrashUploader};
use anyhow::Context;
use libdd_common::Endpoint;
use libdd_telemetry::data::LogLevel;

#[allow(dead_code)]
/// Structured log entry that can be sent via the telemetry log intake.
#[derive(Debug, Clone)]
pub struct LogEntry {
    pub level: LogLevel,
    pub message: String,
    /// Comma-separated tags string (e.g. "service:foo,env:bar").
    pub tags: String,
}

impl LogEntry {
    pub fn new(level: LogLevel, message: impl Into<String>) -> Self {
        Self {
            level,
            message: message.into(),
            tags: String::new(),
        }
    }

    pub fn with_tags(mut self, tags: impl Into<String>) -> Self {
        self.tags = tags.into();
        self
    }
}

/// Minimal uploader that sends log events to the same telemetry log intake used for crash reports.
/// Crashtracking logs need to go to where telemetry uploads go; let's reuse the same uploader
pub struct LogUploader {
    inner: TelemetryCrashUploader,
}

impl LogUploader {
    pub fn new(metadata: &Metadata, endpoint: &Option<Endpoint>) -> anyhow::Result<Self> {
        let inner =
            TelemetryCrashUploader::new(metadata, endpoint).context("creating telemetry logger")?;
        Ok(Self { inner })
    }

    pub async fn send_log(&self, entry: LogEntry) -> anyhow::Result<()> {
        self.inner
            .send_log_payload(entry.message, entry.tags, entry.level)
            .await
    }

    pub async fn send(
        &self,
        level: LogLevel,
        message: impl Into<String>,
        tags: impl Into<String>,
    ) -> anyhow::Result<()> {
        let entry = LogEntry::new(level, message).with_tags(tags);
        self.send_log(entry).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::crash_info::Metadata;
    use libdd_common::Endpoint;
    use libdd_telemetry::data::LogLevel;
    use std::fs;

    #[test]
    fn log_entry_defaults() {
        let entry = LogEntry::new(LogLevel::Debug, "hello");
        assert_eq!(entry.level, LogLevel::Debug);
        assert_eq!(entry.message, "hello");
        assert_eq!(entry.tags, "");
    }

    #[test]
    fn log_entry_with_tags() {
        let entry = LogEntry::new(LogLevel::Error, "msg").with_tags("service:foo,env:bar");
        assert_eq!(entry.tags, "service:foo,env:bar");
    }

    #[tokio::test]
    #[cfg_attr(miri, ignore)]
    async fn send_log_writes_file_payload() -> anyhow::Result<()> {
        let tmp = tempfile::tempdir().unwrap();
        let mut base_path = tmp.keep();
        base_path.push("log_payload");
        let telemetry_path = base_path.with_extension("telemetry");

        let metadata = Metadata::new(
            "libdatadog".to_string(),
            "1.0.0".to_string(),
            "native".to_string(),
            vec![
                "service:foo".to_string(),
                "service_version:bar".to_string(),
                "runtime-id:xyz".to_string(),
                "language:native".to_string(),
            ],
        );

        let uploader = LogUploader::new(
            &metadata,
            &Some(Endpoint::from_slice(&format!(
                "file://{}",
                base_path.to_str().unwrap()
            ))),
        )?;

        uploader
            .send(LogLevel::Warn, "hello log", "service:foo,env:bar")
            .await?;

        let payload: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(&telemetry_path)?)?;

        assert_eq!(payload["api_version"], "v2");
        assert_eq!(payload["request_type"], "logs");
        assert_eq!(payload["origin"], "Crashtracker");

        let log_entry = &payload["payload"][0];
        assert_eq!(log_entry["level"], "WARN");
        assert_eq!(log_entry["is_sensitive"], false);
        assert_eq!(log_entry["is_crash"], false);
        assert_eq!(log_entry["message"], "hello log");
        assert_eq!(log_entry["tags"], "service:foo,env:bar");

        Ok(())
    }
}
