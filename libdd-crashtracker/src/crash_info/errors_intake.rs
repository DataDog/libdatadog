// Copyright 2024-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use std::time::SystemTime;

use crate::SigInfo;

use super::{build_crash_ping_message, CrashInfo, Metadata, StackTrace};
use anyhow::Context;
use chrono::{DateTime, Utc};
use libdd_common::{config::parse_env, parse_uri, Endpoint};
use http::{uri::PathAndQuery, Uri};
use serde::{Deserialize, Serialize};
use std::{borrow::Cow, time::Duration};

pub const DEFAULT_DD_SITE: &str = "datadoghq.com";
pub const PROD_ERRORS_INTAKE_SUBDOMAIN: &str = "error-tracking-intake";

const DIRECT_ERRORS_INTAKE_URL_PATH: &str = "/api/v2/errorsintake";
const AGENT_ERRORS_INTAKE_URL_PATH: &str = "/evp_proxy/v4/api/v2/errorsintake";

const DEFAULT_AGENT_HOST: &str = "localhost";
const DEFAULT_AGENT_PORT: u16 = 8126;

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct ErrorsIntakeConfig {
    pub(crate) endpoint: Option<Endpoint>,
    pub direct_submission_enabled: bool,
    pub debug_enabled: bool,
    pub errors_intake_enabled: bool,
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
    pub errors_intake_enabled: bool,

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

    // Feature flags
    const _DD_ERRORS_INTAKE_ENABLED: &'static str = "_DD_ERRORS_INTAKE_ENABLED";

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
            errors_intake_enabled: parse_env::bool(Self::_DD_ERRORS_INTAKE_ENABLED).unwrap_or(true),

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
        .or_else(|| {
            #[cfg(unix)]
            return settings
                .agent_uds_socket_found
                .then(|| "unix:///var/run/datadog/apm.socket".to_string());
            #[cfg(not(unix))]
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

    pub fn is_errors_intake_enabled(&self) -> bool {
        self.errors_intake_enabled
    }

    pub fn set_endpoint(&mut self, endpoint: Endpoint) -> anyhow::Result<()> {
        self.endpoint = Some(endpoint_with_errors_intake_path(
            endpoint,
            self.direct_submission_enabled,
        )?);
        Ok(())
    }

    pub fn from_settings(settings: &ErrorsIntakeSettings) -> Self {
        let api_key = Self::api_key_from_settings(settings);

        let mut this = Self {
            endpoint: None,
            direct_submission_enabled: settings.direct_submission_enabled,
            debug_enabled: settings.shared_lib_debug,
            errors_intake_enabled: settings.errors_intake_enabled,
        };

        // For direct submission, construct the proper intake URL
        let url = if settings.direct_submission_enabled && settings.api_key.is_some() {
            // Check for explicit errors intake URL first
            if let Some(ref errors_intake_url) = settings.errors_intake_dd_url {
                errors_intake_url.clone()
            } else {
                // Build direct submission URL using site configuration
                let site = settings.site.as_deref().unwrap_or(DEFAULT_DD_SITE);
                format!("https://{}.{}", PROD_ERRORS_INTAKE_SUBDOMAIN, site)
            }
        } else {
            Self::trace_agent_url_from_setting(settings)
        };

        if let Ok(parsed_url) = parse_uri(&url) {
            let _res = this.set_endpoint(Endpoint {
                url: parsed_url,
                api_key,
                ..Default::default()
            });
        }

        this
    }

    pub fn from_env() -> Self {
        let settings = ErrorsIntakeSettings::from_env();
        Self::from_settings(&settings)
    }

    pub fn set_host_from_url(&mut self, host_url: &str) -> anyhow::Result<()> {
        let endpoint = self.endpoint.take().unwrap_or_default();

        self.set_endpoint(Endpoint {
            url: parse_uri(host_url)?,
            ..endpoint
        })
    }
}

#[derive(serde::Serialize, Debug)]
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

#[derive(serde::Serialize, Debug)]
pub struct ErrorsIntakePayload {
    pub timestamp: u64,
    pub ddsource: String,
    pub ddtags: String,
    pub error: ErrorObject,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub trace_id: Option<String>,
}

#[derive(Debug, Default)]
struct ExtractedMetadata {
    service_name: String,
    env: Option<String>,
    service_version: Option<String>,
    language_name: Option<String>,
    language_version: Option<String>,
    tracer_version: Option<String>,
}

impl ExtractedMetadata {
    fn from_metadata(metadata: &Metadata) -> Self {
        let mut result = Self {
            service_name: "unknown".to_string(),
            ..Default::default()
        };

        for tag in &metadata.tags {
            if let Some((key, value)) = tag.split_once(':') {
                match key {
                    "service" => result.service_name = value.to_string(),
                    "env" => result.env = Some(value.to_string()),
                    "version" | "service_version" => {
                        result.service_version = Some(value.to_string())
                    }
                    "language" => result.language_name = Some(value.to_string()),
                    "language_version" | "runtime_version" => {
                        result.language_version = Some(value.to_string())
                    }
                    "library_version" | "profiler_version" => {
                        result.tracer_version = Some(value.to_string())
                    }
                    _ => {}
                }
            }
        }

        result
    }

    fn append_base_tags(&self, tags: &mut String) {
        tags.push_str(&format!("service:{}", self.service_name));
        if let Some(env) = &self.env {
            tags.push_str(&format!(",env:{env}"));
        }
        if let Some(version) = &self.service_version {
            tags.push_str(&format!(",version:{version}"));
        }
    }

    fn append_runtime_tags(&self, tags: &mut String) {
        if let Some(language_name) = &self.language_name {
            tags.push_str(&format!(",language_name:{language_name}"));
        }
        if let Some(language_version) = &self.language_version {
            tags.push_str(&format!(",language_version:{language_version}"));
        }
        if let Some(tracer_version) = &self.tracer_version {
            tags.push_str(&format!(",tracer_version:{tracer_version}"));
        }
    }
}

fn append_signal_tags(tags: &mut String, sig_info: &SigInfo) {
    tags.push_str(&format!(
        ",si_code_human_readable:{:?}",
        sig_info.si_code_human_readable
    ));
    tags.push_str(&format!(",si_signo:{}", sig_info.si_signo));
    tags.push_str(&format!(
        ",si_signo_human_readable:{:?}",
        sig_info.si_signo_human_readable
    ));
}

fn build_crash_info_tags(crash_info: &CrashInfo) -> String {
    let mut tags = format!("data_schema_version:{}", crash_info.data_schema_version);

    if let Some(fingerprint) = &crash_info.fingerprint {
        tags.push_str(&format!(",fingerprint:{fingerprint}"));
    }

    tags.push_str(&format!(",incomplete:{}", crash_info.incomplete));
    tags.push_str(&format!(",is_crash:{}", crash_info.error.is_crash));
    tags.push_str(&format!(",uuid:{}", crash_info.uuid));

    // Add all counters
    for (counter, value) in &crash_info.counters {
        tags.push_str(&format!(",{counter}:{value}"));
    }

    // Add signal information
    if let Some(siginfo) = &crash_info.sig_info {
        if let Some(si_addr) = &siginfo.si_addr {
            tags.push_str(&format!(",si_addr:{si_addr}"));
        }
        tags.push_str(&format!(",si_code:{}", siginfo.si_code));
        append_signal_tags(&mut tags, siginfo);
    }

    tags
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

        let metadata = ExtractedMetadata::from_metadata(&crash_info.metadata);
        let mut ddtags = String::new();
        metadata.append_base_tags(&mut ddtags);
        metadata.append_runtime_tags(&mut ddtags);

        let crash_tags = build_crash_info_tags(crash_info);
        ddtags.push_str(&format!(",{crash_tags}"));

        let (error_type, error_message) = if let Some(sig_info) = &crash_info.sig_info {
            (
                Some(format!("{:?}", sig_info.si_signo_human_readable)),
                Some(format!(
                    "Process terminated by signal {:?}",
                    sig_info.si_signo_human_readable
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

        let extracted_metadata = ExtractedMetadata::from_metadata(metadata);
        let mut ddtags = format!(
            "uuid:{},is_crash_ping:true,service:{}",
            crash_uuid, extracted_metadata.service_name
        );
        extracted_metadata.append_runtime_tags(&mut ddtags);
        if let Some(env) = &extracted_metadata.env {
            ddtags.push_str(&format!(",env:{env}"));
        }
        if let Some(version) = &extracted_metadata.service_version {
            ddtags.push_str(&format!(",version:{version}"));
        }

        append_signal_tags(&mut ddtags, sig_info);

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
    pub fn new(endpoint: &Option<Endpoint>) -> anyhow::Result<Self> {
        let mut cfg = ErrorsIntakeConfig::from_env();

        if let Some(endpoint) = endpoint {
            cfg.set_endpoint(endpoint.clone())?;
        }
        Ok(Self { cfg })
    }

    pub fn is_enabled(&self) -> bool {
        self.cfg.is_errors_intake_enabled()
    }

    pub async fn upload_crash_ping(
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
            return Ok(());
        };

        // Handle file endpoint
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
        assert_eq!(payload.error.source_type, Some("Crashtracking".to_string()));
        assert_eq!(payload.error.is_crash, Some(true));

        let ddtags = &payload.ddtags;

        assert!(ddtags.contains("service:foo"));
        assert!(ddtags.contains("version:bar"));
        assert!(ddtags.contains("language_name:native"));

        assert!(ddtags.contains("data_schema_version:1.4"));
        assert!(ddtags.contains("incomplete:true"));
        assert!(ddtags.contains("is_crash:true"));
        assert!(ddtags.contains("uuid:1d6b97cb-968c-40c9-af6e-e4b4d71e8781"));

        assert!(ddtags.contains("collecting_sample:1"));
        assert!(ddtags.contains("not_profiling:0"));

        assert!(ddtags.contains("si_addr:0x0000000000001234"));
        assert!(ddtags.contains("si_code:1"));
        assert!(ddtags.contains("si_code_human_readable:SEGV_BNDERR"));
        assert!(ddtags.contains("si_signo:11"));
        assert!(ddtags.contains("si_signo_human_readable:SIGSEGV"));
    }

    #[test]
    fn test_errors_payload_from_crash_ping() {
        let metadata = Metadata::test_instance(1);
        let sig_info = crate::SigInfo::test_instance(42);
        let crash_uuid = "test-uuid-123";

        let payload =
            ErrorsIntakePayload::from_crash_ping(crash_uuid, &sig_info, &metadata).unwrap();

        assert_eq!(payload.ddsource, "crashtracker");
        assert_eq!(payload.error.source_type, Some("Crashtracking".to_string()));
        assert_eq!(payload.error.is_crash, Some(false));
        assert!(payload.error.stack.is_none());

        let ddtags = &payload.ddtags;

        assert!(ddtags.contains("uuid:test-uuid-123"));
        assert!(ddtags.contains("is_crash_ping:true"));
        assert!(ddtags.contains("service:foo"));

        assert!(ddtags.contains("language_name:native"));

        assert!(ddtags.contains("version:bar"));

        assert!(ddtags.contains("si_code_human_readable:SEGV_BNDERR"));
        assert!(ddtags.contains("si_signo:11"));
        assert!(ddtags.contains("si_signo_human_readable:SIGSEGV"));
    }

    #[test]
    fn test_errors_intake_has_all_telemetry_tags() {
        let crash_info = CrashInfo::test_instance(1);
        let payload = ErrorsIntakePayload::from_crash_info(&crash_info).unwrap();

        let expected_crash_tags = [
            "data_schema_version:1.4",
            "incomplete:true",
            "is_crash:true",
            "uuid:1d6b97cb-968c-40c9-af6e-e4b4d71e8781",
            "collecting_sample:1",
            "not_profiling:0",
            "si_addr:0x0000000000001234",
            "si_code:1",
            "si_code_human_readable:SEGV_BNDERR",
            "si_signo:11",
            "si_signo_human_readable:SIGSEGV",
        ];

        let expected_metadata_tags = ["service:foo", "version:bar", "language_name:native"];

        for tag in expected_crash_tags
            .iter()
            .chain(expected_metadata_tags.iter())
        {
            assert!(
                payload.ddtags.contains(tag),
                "Missing expected tag: {} in ddtags: {}",
                tag,
                payload.ddtags
            );
        }
    }

    #[test]
    fn test_crash_ping_has_all_telemetry_tags() {
        let metadata = Metadata::test_instance(1);
        let sig_info = crate::SigInfo::test_instance(42);
        let crash_uuid = "test-crash-ping-uuid";

        let payload =
            ErrorsIntakePayload::from_crash_ping(crash_uuid, &sig_info, &metadata).unwrap();

        // This test ensures we have all the tags that telemetry crash ping produces
        let expected_tags = [
            "uuid:test-crash-ping-uuid",
            "is_crash_ping:true",
            "service:foo",
            "language_name:native",
            "version:bar",
            "si_code_human_readable:SEGV_BNDERR",
            "si_signo:11",
            "si_signo_human_readable:SIGSEGV",
        ];

        for tag in expected_tags {
            assert!(
                payload.ddtags.contains(tag),
                "Missing expected tag: {} in ddtags: {}",
                tag,
                payload.ddtags
            );
        }
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

        // Test direct submission configuration
        std::env::set_var("DD_API_KEY", "test-key");
        std::env::set_var("_DD_DIRECT_SUBMISSION_ENABLED", "true");

        let cfg = ErrorsIntakeConfig::from_env();
        let endpoint = cfg.endpoint().unwrap();

        // Should use error-tracking-intake.datadoghq.com for direct submission
        assert_eq!(
            endpoint.url.host(),
            Some("error-tracking-intake.datadoghq.com")
        );
        assert_eq!(endpoint.url.scheme_str(), Some("https"));
        assert!(endpoint.api_key.is_some());

        // With direct submission enabled and API key, should use direct path
        assert_eq!(endpoint.url.path(), DIRECT_ERRORS_INTAKE_URL_PATH);

        std::env::remove_var("DD_API_KEY");
        std::env::remove_var("_DD_DIRECT_SUBMISSION_ENABLED");
    }

    #[test]
    fn test_errors_intake_config_custom_site() {
        let _lock = ENV_TEST_LOCK.lock().unwrap();

        std::env::remove_var("DD_TRACE_AGENT_URL");
        std::env::remove_var("DD_AGENT_HOST");
        std::env::remove_var("DD_TRACE_AGENT_PORT");
        std::env::remove_var("DD_API_KEY");
        std::env::remove_var("_DD_DIRECT_SUBMISSION_ENABLED");
        std::env::remove_var("DD_SITE");

        // Test direct submission with custom site
        std::env::set_var("DD_API_KEY", "test-key");
        std::env::set_var("_DD_DIRECT_SUBMISSION_ENABLED", "true");
        std::env::set_var("DD_SITE", "us3.datadoghq.com");

        let cfg = ErrorsIntakeConfig::from_env();
        let endpoint = cfg.endpoint().unwrap();

        // Should use error-tracking-intake with custom site
        assert_eq!(
            endpoint.url.host(),
            Some("error-tracking-intake.us3.datadoghq.com")
        );
        assert_eq!(endpoint.url.scheme_str(), Some("https"));
        assert!(endpoint.api_key.is_some());
        assert_eq!(endpoint.url.path(), DIRECT_ERRORS_INTAKE_URL_PATH);

        std::env::remove_var("DD_API_KEY");
        std::env::remove_var("_DD_DIRECT_SUBMISSION_ENABLED");
        std::env::remove_var("DD_SITE");
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

        std::env::set_var("DD_TRACE_AGENT_URL", "http://localhost:9126");

        let cfg = ErrorsIntakeConfig::from_env();
        let endpoint = cfg.endpoint().unwrap();

        assert_eq!(endpoint.url.host(), Some("localhost"));
        assert_eq!(endpoint.url.port_u16(), Some(9126));

        // Should use agent proxy path
        assert_eq!(endpoint.url.path(), AGENT_ERRORS_INTAKE_URL_PATH);

        std::env::remove_var("DD_TRACE_AGENT_URL");
        std::env::remove_var("DD_AGENT_HOST");
        std::env::remove_var("DD_TRACE_AGENT_PORT");
        std::env::remove_var("DD_API_KEY");
        std::env::remove_var("_DD_DIRECT_SUBMISSION_ENABLED");
        std::env::remove_var("DD_SITE");
    }

    #[test]
    fn test_errors_intake_config_agent_with_api_key_but_no_direct() {
        let _lock = ENV_TEST_LOCK.lock().unwrap();

        std::env::remove_var("DD_TRACE_AGENT_URL");
        std::env::remove_var("DD_AGENT_HOST");
        std::env::remove_var("DD_TRACE_AGENT_PORT");
        std::env::remove_var("DD_API_KEY");
        std::env::remove_var("_DD_DIRECT_SUBMISSION_ENABLED");
        std::env::remove_var("DD_SITE");

        // API key is set but direct submission is NOT enabled
        // Should still use agent proxy
        std::env::set_var("DD_TRACE_AGENT_URL", "http://localhost:9126");
        std::env::set_var("DD_API_KEY", "test-key");

        let cfg = ErrorsIntakeConfig::from_env();
        let endpoint = cfg.endpoint().unwrap();

        // Should use agent URL, not direct submission
        assert_eq!(endpoint.url.host(), Some("localhost"));
        assert_eq!(endpoint.url.port_u16(), Some(9126));

        // Should use agent proxy path, not direct path
        assert_eq!(endpoint.url.path(), AGENT_ERRORS_INTAKE_URL_PATH);
        assert!(endpoint.api_key.is_none());

        std::env::remove_var("DD_TRACE_AGENT_URL");
        std::env::remove_var("DD_API_KEY");
    }

    #[test]
    fn test_errors_intake_enabled_flag() {
        let _lock = ENV_TEST_LOCK.lock().unwrap();

        // Test default behavior (should be enabled)
        std::env::remove_var("_DD_ERRORS_INTAKE_ENABLED");
        let cfg = ErrorsIntakeConfig::from_env();
        assert!(cfg.is_errors_intake_enabled());

        // Test explicitly enabled
        std::env::set_var("_DD_ERRORS_INTAKE_ENABLED", "true");
        let cfg = ErrorsIntakeConfig::from_env();
        assert!(cfg.is_errors_intake_enabled());

        // Test explicitly disabled
        std::env::set_var("_DD_ERRORS_INTAKE_ENABLED", "false");
        let cfg = ErrorsIntakeConfig::from_env();
        assert!(!cfg.is_errors_intake_enabled());

        // Test with uploader
        let uploader = ErrorsIntakeUploader::new(&None).unwrap();
        assert!(!uploader.is_enabled());

        std::env::set_var("_DD_ERRORS_INTAKE_ENABLED", "true");
        let uploader = ErrorsIntakeUploader::new(&None).unwrap();
        assert!(uploader.is_enabled());

        std::env::remove_var("_DD_ERRORS_INTAKE_ENABLED");
    }
}
