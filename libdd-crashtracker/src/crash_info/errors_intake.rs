// Copyright 2024-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use std::time::SystemTime;

use crate::SigInfo;

use super::{build_crash_ping_message, CrashInfo, Metadata, StackTrace};
use anyhow::Context;
use chrono::{DateTime, Utc};
use libdd_common::{config::parse_env, parse_uri, Endpoint};
use http::{uri::PathAndQuery, Uri};
use serde::Serialize;
use std::{borrow::Cow, time::Duration};

pub const DEFAULT_DD_SITE: &str = "datad0g.com";
pub const PROD_ERRORS_INTAKE_SUBDOMAIN: &str = "event-platform-intake";

const DIRECT_ERRORS_INTAKE_URL_PATH: &str = "/api/v2/errorsintake";
const AGENT_ERRORS_INTAKE_URL_PATH: &str = "/evp_proxy/v4/api/v2/errorsintake";

const DEFAULT_AGENT_HOST: &str = "localhost";
const DEFAULT_AGENT_PORT: u16 = 8126;

#[derive(Clone, Debug, Default, serde::Serialize, serde::Deserialize)]
pub struct ErrorsIntakeConfig {
    /// Endpoint to send the data to
    /// This is private and should be interacted with through the set_endpoint function
    /// to ensure the url path is properly set
    pub(crate) endpoint: Option<Endpoint>,
    pub direct_submission_enabled: bool,
    pub debug_enabled: bool,
}

fn endpoint_with_errors_intake_path(
    mut endpoint: Endpoint,
    direct_submission_enabled: bool,
) -> anyhow::Result<Endpoint> {
    let mut uri_parts = endpoint.url.into_parts();
    if uri_parts
        .scheme
        .as_ref()
        .is_some_and(|scheme| scheme.as_str() != "file")
    {
        uri_parts.path_and_query = Some(PathAndQuery::from_static(
            if endpoint.api_key.is_some() && direct_submission_enabled {
                DIRECT_ERRORS_INTAKE_URL_PATH
            } else {
                AGENT_ERRORS_INTAKE_URL_PATH
            },
        ));
    }

    endpoint.url = Uri::from_parts(uri_parts)?;
    Ok(endpoint)
}

/// Settings gathers configuration options we receive from the environment
#[derive(Debug, Default)]
pub struct ErrorsIntakeSettings {
    // Env parameter
    pub agent_host: Option<String>,
    pub trace_agent_port: Option<u16>,
    pub trace_agent_url: Option<String>,
    pub trace_pipe_name: Option<String>,
    pub direct_submission_enabled: bool,
    pub api_key: Option<String>,
    pub site: Option<String>,
    pub errors_intake_dd_url: Option<String>,
    pub shared_lib_debug: bool,

    // Filesystem check
    pub agent_uds_socket_found: bool,
}

impl ErrorsIntakeSettings {
    // Agent connection configuration
    const DD_TRACE_AGENT_URL: &'static str = "DD_TRACE_AGENT_URL";
    const DD_AGENT_HOST: &'static str = "DD_AGENT_HOST";
    const DD_TRACE_AGENT_PORT: &'static str = "DD_TRACE_AGENT_PORT";
    const DD_TRACE_PIPE_NAME: &'static str = "DD_TRACE_PIPE_NAME";

    // Direct submission configuration
    const _DD_DIRECT_SUBMISSION_ENABLED: &'static str = "_DD_DIRECT_SUBMISSION_ENABLED";
    const DD_API_KEY: &'static str = "DD_API_KEY";
    const DD_SITE: &'static str = "DD_SITE";
    const DD_ERRORS_INTAKE_DD_URL: &'static str = "DD_ERRORS_INTAKE_DD_URL";

    // Debug configuration
    const _DD_SHARED_LIB_DEBUG: &'static str = "_DD_SHARED_LIB_DEBUG";

    pub fn from_env() -> Self {
        let default = Self::default();
        Self {
            agent_host: parse_env::str_not_empty(Self::DD_AGENT_HOST),
            trace_agent_port: parse_env::int(Self::DD_TRACE_AGENT_PORT),
            trace_agent_url: parse_env::str_not_empty(Self::DD_TRACE_AGENT_URL)
                .or(default.trace_agent_url),
            trace_pipe_name: parse_env::str_not_empty(Self::DD_TRACE_PIPE_NAME)
                .or(default.trace_pipe_name),
            direct_submission_enabled: parse_env::bool(Self::_DD_DIRECT_SUBMISSION_ENABLED)
                .unwrap_or(default.direct_submission_enabled),
            api_key: parse_env::str_not_empty(Self::DD_API_KEY),
            site: parse_env::str_not_empty(Self::DD_SITE),
            errors_intake_dd_url: parse_env::str_not_empty(Self::DD_ERRORS_INTAKE_DD_URL),
            shared_lib_debug: parse_env::bool(Self::_DD_SHARED_LIB_DEBUG).unwrap_or(false),

            agent_uds_socket_found: (|| {
                #[cfg(unix)]
                return std::fs::metadata("/var/run/datadog/apm.socket").is_ok();
                #[cfg(not(unix))]
                return false;
            })(),
        }
    }
}

impl ErrorsIntakeConfig {
    // Implemented following same pattern as telemetry
    fn trace_agent_url_from_setting(settings: &ErrorsIntakeSettings) -> String {
        None.or_else(|| {
            settings
                .trace_agent_url
                .as_deref()
                .filter(|u| {
                    u.starts_with("unix://")
                        || u.starts_with("http://")
                        || u.starts_with("https://")
                })
                .map(ToString::to_string)
        })
        .or_else(|| {
            #[cfg(windows)]
            return settings
                .trace_pipe_name
                .as_ref()
                .map(|pipe_name| format!("windows:{pipe_name}"));
            #[cfg(not(windows))]
            return None;
        })
        .or_else(|| match (&settings.agent_host, settings.trace_agent_port) {
            (None, None) => None,
            _ => Some(format!(
                "http://{}:{}",
                settings.agent_host.as_deref().unwrap_or(DEFAULT_AGENT_HOST),
                settings.trace_agent_port.unwrap_or(DEFAULT_AGENT_PORT),
            )),
        })
        .or_else(|| {
            #[cfg(unix)]
            return settings
                .agent_uds_socket_found
                .then(|| "unix:///var/run/datadog/apm.socket".to_string());
            #[cfg(not(unix))]
            return None;
        })
        .unwrap_or_else(|| format!("http://{DEFAULT_AGENT_HOST}:{DEFAULT_AGENT_PORT}"))
    }

    fn api_key_from_settings(settings: &ErrorsIntakeSettings) -> Option<Cow<'static, str>> {
        if !settings.direct_submission_enabled {
            return None;
        }
        settings.api_key.clone().map(Cow::Owned)
    }

    pub fn endpoint(&self) -> Option<&Endpoint> {
        self.endpoint.as_ref()
    }

    pub fn set_endpoint(&mut self, endpoint: Endpoint) -> anyhow::Result<()> {
        self.endpoint = Some(endpoint_with_errors_intake_path(
            endpoint,
            self.direct_submission_enabled,
        )?);
        Ok(())
    }

    pub fn from_settings(settings: &ErrorsIntakeSettings) -> Self {
        let trace_agent_url = Self::trace_agent_url_from_setting(settings);
        let api_key = Self::api_key_from_settings(settings);

        let mut this = Self {
            endpoint: None,
            direct_submission_enabled: settings.direct_submission_enabled,
            debug_enabled: settings.shared_lib_debug,
        };

        if let Ok(url) = parse_uri(&trace_agent_url) {
            let _res = this.set_endpoint(Endpoint {
                url,
                api_key,
                ..Default::default()
            });
        }

        this
    }

    /// Get the configuration of the errors intake from env variables
    pub fn from_env() -> Self {
        let settings = ErrorsIntakeSettings::from_env();
        Self::from_settings(&settings)
    }

    /// set_host sets the host errors intake should connect to.
    pub fn set_host_from_url(&mut self, host_url: &str) -> anyhow::Result<()> {
        let endpoint = self.endpoint.take().unwrap_or_default();

        self.set_endpoint(Endpoint {
            url: parse_uri(host_url)?,
            ..endpoint
        })
    }
}

#[derive(Serialize, Debug)]
pub struct ErrorObject {
    #[serde(rename = "type", skip_serializing_if = "Option::is_none")]
    pub error_type: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stack: Option<StackTrace>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub is_crash: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fingerprint: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source_type: Option<String>,
}

#[derive(Serialize, Debug)]
pub struct ErrorsIntakePayload {
    pub timestamp: u64,
    pub ddsource: String,
    pub ddtags: String,
    pub error: ErrorObject,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub trace_id: Option<String>,
}

impl ErrorsIntakePayload {
    pub fn from_crash_info(crash_info: &CrashInfo) -> anyhow::Result<Self> {
        let timestamp = crash_info.timestamp.parse::<DateTime<Utc>>().map_or_else(
            |_| {
                SystemTime::now()
                    .duration_since(SystemTime::UNIX_EPOCH)
                    .map(|d| d.as_millis() as u64)
                    .unwrap_or(0)
            },
            |ts| ts.timestamp_millis() as u64,
        );

        // Extract service information from metadata tags
        let mut service_name = "unknown".to_string();
        let mut env = None;
        let mut service_version = None;

        for tag in &crash_info.metadata.tags {
            if let Some((key, value)) = tag.split_once(':') {
                match key {
                    "service" => service_name = value.to_string(),
                    "env" => env = Some(value.to_string()),
                    "version" => service_version = Some(value.to_string()),
                    _ => {}
                }
            }
        }

        // Build ddtags
        let mut ddtags = format!("service:{}", service_name);
        if let Some(env) = env {
            ddtags.push_str(&format!(",env:{env}"));
        }
        if let Some(version) = service_version {
            ddtags.push_str(&format!(",version:{version}"));
        }
        ddtags.push_str(&format!(",uuid:{}", crash_info.uuid));

        // Extract error info from signal
        let (error_type, error_message) = if let Some(sig_info) = &crash_info.sig_info {
            (
                Some(format!("{:?}", sig_info.si_signo_human_readable)),
                Some(format!(
                    "Process terminated with {:?} ({:?})",
                    sig_info.si_code_human_readable, sig_info.si_signo_human_readable
                )),
            )
        } else {
            (
                Some("Unknown".to_string()),
                crash_info.error.message.clone(),
            )
        };

        // Use crash stack if available
        let error_stack = if !crash_info.error.stack.frames.is_empty() {
            Some(crash_info.error.stack.clone())
        } else {
            None
        };

        Ok(Self {
            timestamp,
            ddsource: "crashtracker".to_string(),
            ddtags,
            error: ErrorObject {
                error_type,
                message: error_message,
                stack: error_stack,
                is_crash: Some(true),
                fingerprint: crash_info.fingerprint.clone(),
                source_type: Some("Crashtracking".to_string()),
            },
            trace_id: None,
        })
    }

    pub fn from_crash_ping(
        crash_uuid: &str,
        sig_info: &SigInfo,
        metadata: &Metadata,
    ) -> anyhow::Result<Self> {
        let timestamp = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .map(|d| d.as_millis() as u64)
            .unwrap_or(0);

        // Extract service info from metadata tags
        let mut service_name = "unknown".to_string();
        let mut env = None;
        let mut service_version = None;

        for tag in &metadata.tags {
            if let Some((key, value)) = tag.split_once(':') {
                match key {
                    "service" => service_name = value.to_string(),
                    "env" => env = Some(value.to_string()),
                    "version" => service_version = Some(value.to_string()),
                    _ => {}
                }
            }
        }

        // Build ddtags
        let mut ddtags = format!("service:{}", service_name);
        if let Some(env) = env {
            ddtags.push_str(&format!(",env:{env}"));
        }
        if let Some(version) = service_version {
            ddtags.push_str(&format!(",version:{version}"));
        }
        ddtags.push_str(&format!(",uuid:{crash_uuid}"));
        ddtags.push_str(",is_crash_ping:true");

        Ok(Self {
            timestamp,
            ddsource: "crashtracker".to_string(),
            ddtags,
            error: ErrorObject {
                error_type: Some(format!("{:?}", sig_info.si_signo_human_readable)),
                message: Some(build_crash_ping_message(sig_info)),
                stack: None,
                is_crash: Some(false),
                fingerprint: None,
                source_type: Some("Crashtracking".to_string()),
            },
            trace_id: None,
        })
    }
}

pub struct ErrorsIntakeUploader {
    cfg: ErrorsIntakeConfig,
}

impl ErrorsIntakeUploader {
    pub fn new(
        _crashtracker_metadata: &Metadata,
        _endpoint: &Option<Endpoint>,
    ) -> anyhow::Result<Self> {
        let cfg = ErrorsIntakeConfig::from_env();
        Ok(Self { cfg })
    }

    pub async fn send_crash_ping(
        &self,
        crash_uuid: &str,
        sig_info: &SigInfo,
        metadata: &Metadata,
    ) -> anyhow::Result<()> {
        let payload = ErrorsIntakePayload::from_crash_ping(crash_uuid, sig_info, metadata)?;
        self.send_payload(&payload).await
    }

    pub async fn upload_to_errors_intake(&self, crash_info: &CrashInfo) -> anyhow::Result<()> {
        let payload = ErrorsIntakePayload::from_crash_info(crash_info)?;
        self.send_payload(&payload).await
    }

    async fn send_payload(&self, payload: &ErrorsIntakePayload) -> anyhow::Result<()> {
        let Some(endpoint) = self.cfg.endpoint() else {
            // No endpoint configured - this is fine, errors intake is optional
            return Ok(());
        };

        // Handle file endpoint for testing
        if endpoint.url.scheme_str() == Some("file") {
            let path = libdd_common::decode_uri_path_in_authority(&endpoint.url)
                .context("errors intake file path is not valid")?;

            let file_path = path.with_extension("errors");
            let file = std::fs::File::create(&file_path).with_context(|| {
                format!(
                    "Failed to create errors intake file {}",
                    file_path.display()
                )
            })?;

            serde_json::to_writer_pretty(file, payload).with_context(|| {
                format!(
                    "Failed to write errors intake JSON to {}",
                    file_path.display()
                )
            })?;

            return Ok(());
        }

        // Build HTTP request using the same pattern as telemetry
        let mut req_builder =
            endpoint.to_request_builder(concat!("crashtracker/", env!("CARGO_PKG_VERSION")))?;

        // Add errors intake specific headers
        if endpoint.api_key.is_some() {
            // Direct intake - DD-API-KEY is added by to_request_builder
        } else {
            // Agent proxy - add EvP subdomain header
            req_builder =
                req_builder.header("X-Datadog-EVP-Subdomain", PROD_ERRORS_INTAKE_SUBDOMAIN);
        }

        let req = req_builder
            .method(http::Method::POST)
            .header(
                http::header::CONTENT_TYPE,
                libdd_common::header::APPLICATION_JSON,
            )
            .body(serde_json::to_string(payload)?.into())?;

        // Create HTTP client and send request
        let client = libdd_common::hyper_migration::new_client_periodic();

        tokio::time::timeout(
            Duration::from_millis(endpoint.timeout_ms),
            client.request(req),
        )
        .await??;

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::crash_info::test_utils::TestInstance;
    use std::sync::Mutex;

    // Mutex to ensure environment variable tests run sequentially
    static ENV_TEST_LOCK: Mutex<()> = Mutex::new(());

    #[test]
    fn test_errors_payload_from_crash_info() {
        let crash_info = CrashInfo::test_instance(1);
        let payload = ErrorsIntakePayload::from_crash_info(&crash_info).unwrap();

        assert_eq!(payload.ddsource, "crashtracker");
        assert!(payload.ddtags.contains("service:foo"));
        assert!(payload.ddtags.contains("uuid:"));
        assert_eq!(payload.error.source_type, Some("Crashtracking".to_string()));
        assert_eq!(payload.error.is_crash, Some(true));
    }

    #[test]
    fn test_errors_payload_from_crash_ping() {
        let metadata = Metadata::test_instance(1);
        let sig_info = crate::SigInfo::test_instance(42);
        let crash_uuid = "test-uuid-123";

        let payload =
            ErrorsIntakePayload::from_crash_ping(crash_uuid, &sig_info, &metadata).unwrap();

        assert_eq!(payload.ddsource, "crashtracker");
        assert!(payload.ddtags.contains("service:foo"));
        assert!(payload.ddtags.contains("uuid:test-uuid-123"));
        assert!(payload.ddtags.contains("is_crash_ping:true"));
        assert_eq!(payload.error.source_type, Some("Crashtracking".to_string()));
        assert_eq!(payload.error.is_crash, Some(false));
        assert!(payload.error.stack.is_none());
    }

    #[test]
    fn test_errors_intake_config_from_env() {
        let _lock = ENV_TEST_LOCK.lock().unwrap();

        // Clear all environment variables first to isolate test
        std::env::remove_var("DD_TRACE_AGENT_URL");
        std::env::remove_var("DD_AGENT_HOST");
        std::env::remove_var("DD_TRACE_AGENT_PORT");
        std::env::remove_var("DD_API_KEY");
        std::env::remove_var("_DD_DIRECT_SUBMISSION_ENABLED");
        std::env::remove_var("DD_SITE");

        // Test configuration building from environment
        std::env::set_var("DD_AGENT_HOST", "test-host");
        std::env::set_var("DD_TRACE_AGENT_PORT", "1234");
        std::env::set_var("DD_API_KEY", "test-key");
        std::env::set_var("_DD_DIRECT_SUBMISSION_ENABLED", "true");

        let cfg = ErrorsIntakeConfig::from_env();
        let endpoint = cfg.endpoint().unwrap();

        assert_eq!(endpoint.url.host(), Some("test-host"));
        assert_eq!(endpoint.url.port_u16(), Some(1234));
        assert!(endpoint.api_key.is_some());

        // With direct submission enabled and API key, should use direct path
        assert_eq!(endpoint.url.path(), DIRECT_ERRORS_INTAKE_URL_PATH);

        std::env::remove_var("DD_AGENT_HOST");
        std::env::remove_var("DD_TRACE_AGENT_PORT");
        std::env::remove_var("DD_API_KEY");
        std::env::remove_var("_DD_DIRECT_SUBMISSION_ENABLED");
    }

    #[test]
    fn test_errors_intake_config_agent_proxy() {
        let _lock = ENV_TEST_LOCK.lock().unwrap();

        // Clear all environment variables first to isolate test
        std::env::remove_var("DD_TRACE_AGENT_URL");
        std::env::remove_var("DD_AGENT_HOST");
        std::env::remove_var("DD_TRACE_AGENT_PORT");
        std::env::remove_var("DD_API_KEY");
        std::env::remove_var("_DD_DIRECT_SUBMISSION_ENABLED");
        std::env::remove_var("DD_SITE");

        // Test agent proxy configuration (no API key or direct submission disabled)
        std::env::set_var("DD_TRACE_AGENT_URL", "http://localhost:9126");

        let cfg = ErrorsIntakeConfig::from_env();
        let endpoint = cfg.endpoint().unwrap();

        assert_eq!(endpoint.url.host(), Some("localhost"));
        assert_eq!(endpoint.url.port_u16(), Some(9126));

        // Should use agent proxy path
        assert_eq!(endpoint.url.path(), AGENT_ERRORS_INTAKE_URL_PATH);

        // Clean up test environment
        std::env::remove_var("DD_TRACE_AGENT_URL");
        std::env::remove_var("DD_AGENT_HOST");
        std::env::remove_var("DD_TRACE_AGENT_PORT");
        std::env::remove_var("DD_API_KEY");
        std::env::remove_var("_DD_DIRECT_SUBMISSION_ENABLED");
        std::env::remove_var("DD_SITE");
    }
}
