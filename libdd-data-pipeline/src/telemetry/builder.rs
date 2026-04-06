// Copyright 2024-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

#[cfg(feature = "telemetry")]
use super::metrics::Metrics;
use super::worker;
use super::TelemetryClient;
#[cfg(feature = "telemetry")]
use libdd_telemetry::worker::{TelemetryWorkerBuilder, TelemetryWorkerFlavor};
use std::time::Duration;
use tokio::runtime::Handle;
use super::error::TelemetryError;

/// Structure to build a Telemetry client.
///
/// Holds partial data until the `build` method is called which results in a new
/// `TelemetryClient`.
#[derive(Default)]
pub struct TelemetryClientBuilder {
    service_name: Option<String>,
    service_version: Option<String>,
    env: Option<String>,
    language: Option<String>,
    language_version: Option<String>,
    tracer_version: Option<String>,
    url: Option<String>,
    heartbeat: Option<Duration>,
    debug_enabled: bool,
    runtime_id: Option<String>,
}

impl TelemetryClientBuilder {
    /// Sets the service name for the telemetry client
    pub fn set_service_name(mut self, name: &str) -> Self {
        self.service_name = Some(name.to_string());
        self
    }

    /// Sets the service version for the telemetry client
    pub fn set_service_version(mut self, version: &str) -> Self {
        self.service_version = Some(version.to_string());
        self
    }

    /// Sets the env name for the telemetry client
    pub fn set_env(mut self, name: &str) -> Self {
        self.env = Some(name.to_string());
        self
    }

    /// Sets the language name for the telemetry client
    pub fn set_language(mut self, lang: &str) -> Self {
        self.language = Some(lang.to_string());
        self
    }

    /// Sets the language version for the telemetry client
    pub fn set_language_version(mut self, version: &str) -> Self {
        self.language_version = Some(version.to_string());
        self
    }

    /// Sets the tracer version for the telemetry client
    pub fn set_tracer_version(mut self, version: &str) -> Self {
        self.tracer_version = Some(version.to_string());
        self
    }

    /// Sets the url where the metrics will be sent.
    pub fn set_url(mut self, url: &str) -> Self {
        self.url = Some(url.to_string());
        self
    }

    /// Sets the heartbeat notification interval in millis.
    pub fn set_heartbeat(mut self, interval: u64) -> Self {
        if interval > 0 {
            self.heartbeat = Some(Duration::from_millis(interval));
        }
        self
    }

    /// Sets runtime id for the telemetry client.
    pub fn set_runtime_id(mut self, id: &str) -> Self {
        self.runtime_id = Some(id.to_string());
        self
    }

    /// Sets the debug enabled flag for the telemetry client.
    pub fn set_debug_enabled(mut self, debug: bool) -> Self {
        self.debug_enabled = debug;
        self
    }
}

#[cfg(feature = "telemetry")]
impl TelemetryClientBuilder {
    /// Builds the telemetry client.
    pub fn build(self, runtime: Handle) -> Result<(TelemetryClient, worker::TelemetryWorker), TelemetryError> {
        let mut builder = TelemetryWorkerBuilder::new_fetch_host(
            self.service_name.ok_or_else(|| TelemetryError::Builder("service_name is required".to_string()))?,
            self.language.ok_or_else(|| TelemetryError::Builder("language is required".to_string()))?,
            self.language_version.ok_or_else(|| TelemetryError::Builder("language_version is required".to_string()))?,
            self.tracer_version.ok_or_else(|| TelemetryError::Builder("tracer_version is required".to_string()))?,
        );
        if let Some(url) = self.url {
            builder
                .config
                .set_endpoint(libdd_common::Endpoint::from_slice(&url))
                .map_err(|e| TelemetryError::Builder(e.to_string()))?
        }
        if let Some(heartbeat) = self.heartbeat {
            builder.config.telemetry_heartbeat_interval = heartbeat;
        }
        builder.config.debug_enabled = self.debug_enabled;
        // Send only metrics and logs and drop lifecycle events
        builder.flavor = TelemetryWorkerFlavor::MetricsLogs;
        builder.application.env = self.env;
        builder.application.service_version = self.service_version;

        if let Some(id) = self.runtime_id {
            builder.runtime_id = Some(id);
        }

        let (worker_handle, worker) = builder.build_worker(runtime);

        Ok((
            TelemetryClient {
                metrics: Metrics::new(&worker_handle),
                worker: worker_handle,
            },
            worker,
        ))
    }
}

#[cfg(not(feature = "telemetry"))]
impl TelemetryClientBuilder {
    /// Builds a no-op telemetry client.
    pub fn build(self, _runtime: Handle) -> Result<(TelemetryClient, worker::TelemetryWorker), TelemetryError> {
        Ok((TelemetryClient {}, worker::TelemetryWorker {}))
    }
}

#[cfg(test)]
#[cfg(feature = "telemetry")]
mod tests {
    use super::*;

    #[test]
    fn builder_test_default() {
        let builder = TelemetryClientBuilder::default();

        assert!(builder.service_name.is_none());
        assert!(builder.service_version.is_none());
        assert!(builder.env.is_none());
        assert!(builder.language.is_none());
        assert!(builder.language_version.is_none());
        assert!(builder.tracer_version.is_none());
        assert!(!builder.debug_enabled);
        assert!(builder.url.is_none());
        assert!(builder.heartbeat.is_none());
    }

    #[test]
    fn builder_test() {
        let builder = TelemetryClientBuilder::default()
            .set_service_name("test_service")
            .set_service_version("test_version")
            .set_env("test_env")
            .set_language("test_language")
            .set_language_version("test_language_version")
            .set_tracer_version("test_tracer_version")
            .set_url("http://localhost")
            .set_debug_enabled(true)
            .set_heartbeat(30);

        assert_eq!(&builder.service_name.unwrap(), "test_service");
        assert_eq!(&builder.service_version.unwrap(), "test_version");
        assert_eq!(&builder.env.unwrap(), "test_env");
        assert_eq!(&builder.language.unwrap(), "test_language");
        assert_eq!(&builder.language_version.unwrap(), "test_language_version");
        assert_eq!(&builder.tracer_version.unwrap(), "test_tracer_version");
        assert!(builder.debug_enabled);
        assert_eq!(builder.url.as_deref(), Some("http://localhost"));
        assert_eq!(builder.heartbeat.unwrap(), Duration::from_millis(30));
    }
}
